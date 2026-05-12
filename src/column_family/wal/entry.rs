use crate::Durability;
use crate::tree_store::{Checksum, PageNumber};
use std::io;

/// A single key-value operation for deferred flush WAL entries.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WALOp {
    pub(crate) key: Vec<u8>,
    pub(crate) value: Option<Vec<u8>>, // None = delete
}

/// Payload for deferred flush: stores logical key-value operations.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WALLogicalOpsPayload {
    pub(crate) table_name: String,
    pub(crate) ops: Vec<WALOp>,
}

/// Discriminated payload for WAL entries.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum WALPayload {
    /// B-tree state snapshot (existing, used by normal commits and checkpoints)
    Transaction(WALTransactionPayload),
    /// Logical operations (new, used by deferred flush commits)
    LogicalOps(WALLogicalOpsPayload),
}

/// A single entry in the Write-Ahead Log.
///
/// Each entry represents a committed transaction that has been durably written
/// to the WAL but may not yet have been applied to the main database file.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WALEntry {
    /// Monotonic sequence number assigned by the journal.
    pub(crate) sequence: u64,

    /// Name of the column family this transaction belongs to.
    pub(crate) cf_name: String,

    /// Transaction ID from the underlying redb `TransactionalMemory`.
    pub(crate) transaction_id: u64,

    /// The serialized transaction payload.
    pub(crate) payload: WALPayload,
}

/// The payload of a WAL entry containing all information needed to replay a transaction.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WALTransactionPayload {
    /// Root of the user data B-tree after this transaction.
    /// Stored as (`PageNumber`, Checksum, length) to fully reconstruct `BtreeHeader`.
    pub(crate) user_root: Option<(PageNumber, Checksum, u64)>,

    /// Root of the system B-tree after this transaction.
    /// Stored as (`PageNumber`, Checksum, length) to fully reconstruct `BtreeHeader`.
    pub(crate) system_root: Option<(PageNumber, Checksum, u64)>,

    /// Pages freed by this transaction.
    pub(crate) freed_pages: Vec<PageNumber>,

    /// Pages allocated by this transaction.
    pub(crate) allocated_pages: Vec<PageNumber>,

    /// Original durability setting of the transaction.
    pub(crate) durability: Durability,
}

impl WALEntry {
    /// Creates a new WAL entry.
    ///
    /// The sequence number will be assigned by the journal during append.
    pub(crate) fn new(
        cf_name: String,
        transaction_id: u64,
        payload: WALTransactionPayload,
    ) -> Self {
        Self {
            sequence: 0, // Will be assigned by journal
            cf_name,
            transaction_id,
            payload: WALPayload::Transaction(payload),
        }
    }

    pub(crate) fn new_logical_ops(
        cf_name: String,
        transaction_id: u64,
        table_name: String,
        ops: Vec<WALOp>,
    ) -> Self {
        Self {
            sequence: 0, // Will be assigned by journal
            cf_name,
            transaction_id,
            payload: WALPayload::LogicalOps(WALLogicalOpsPayload { table_name, ops }),
        }
    }

    /// Serializes the entry to bytes using zero-cost manual serialization.
    ///
    /// Format:
    /// - sequence: u64 (8 bytes)
    /// - `cf_name_len`: u32 (4 bytes)
    /// - `cf_name`: [u8; `cf_name_len`] (variable)
    /// - `transaction_id`: u64 (8 bytes)
    /// - payload: serialized `WALTransactionPayload` (variable)
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        // Sequence number
        buf.extend_from_slice(&self.sequence.to_le_bytes());

        // CF name (length-prefixed string)
        let cf_name_bytes = self.cf_name.as_bytes();
        #[allow(clippy::cast_possible_truncation)]
        buf.extend_from_slice(&(cf_name_bytes.len() as u32).to_le_bytes());
        buf.extend_from_slice(cf_name_bytes);

        // Transaction ID
        buf.extend_from_slice(&self.transaction_id.to_le_bytes());

        // Payload (with type discriminator)
        match &self.payload {
            WALPayload::Transaction(txn) => {
                buf.push(0x01);
                txn.serialize_into(&mut buf);
            }
            WALPayload::LogicalOps(ops) => {
                buf.push(0x02);
                ops.serialize_into(&mut buf);
            }
        }

        buf
    }

    /// Deserializes an entry from bytes.
    ///
    /// Returns the entry and the number of bytes consumed.
    pub(crate) fn from_bytes(data: &[u8]) -> io::Result<(Self, usize)> {
        let mut offset = 0;

        // Read sequence
        if data.len() < offset + 8 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated sequence",
            ));
        }
        let sequence = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
        offset += 8;

        // Read CF name length
        if data.len() < offset + 4 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated cf_name length",
            ));
        }
        let cf_name_len = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;

        // Read CF name
        if data.len() < offset + cf_name_len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated cf_name",
            ));
        }
        let cf_name =
            String::from_utf8(data[offset..offset + cf_name_len].to_vec()).map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidData, format!("invalid UTF-8: {e}"))
            })?;
        offset += cf_name_len;

        // Read transaction ID
        if data.len() < offset + 8 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated transaction_id",
            ));
        }
        let transaction_id = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
        offset += 8;

        // Read payload type discriminator
        if data.len() < offset + 1 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated payload type discriminator",
            ));
        }
        let payload_type = data[offset];
        offset += 1;

        let payload = match payload_type {
            0x01 => {
                let (txn, len) = WALTransactionPayload::deserialize_from(&data[offset..])?;
                offset += len;
                WALPayload::Transaction(txn)
            }
            0x02 => {
                let (ops, len) = WALLogicalOpsPayload::deserialize_from(&data[offset..])?;
                offset += len;
                WALPayload::LogicalOps(ops)
            }
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unknown WAL payload type: {other:#x}"),
                ));
            }
        };

        Ok((
            Self {
                sequence,
                cf_name,
                transaction_id,
                payload,
            },
            offset,
        ))
    }
}

impl WALTransactionPayload {
    /// Serializes the payload into the given buffer.
    ///
    /// Format:
    /// - `user_root_present`: u8 (1 = present, 0 = None)
    /// - `user_root`: `PageNumber` (8 bytes) + Checksum (16 bytes) + length (8 bytes) if present
    /// - `system_root_present`: u8
    /// - `system_root`: `PageNumber` + Checksum + length if present
    /// - `freed_pages_count`: u32 (4 bytes)
    /// - `freed_pages`: [`PageNumber`; count] (8 bytes each)
    /// - `allocated_pages_count`: u32 (4 bytes)
    /// - `allocated_pages`: [`PageNumber`; count] (8 bytes each)
    /// - durability: u8 (1 byte)
    fn serialize_into(&self, buf: &mut Vec<u8>) {
        // User root
        if let Some((page_num, checksum, length)) = self.user_root {
            buf.push(1);
            buf.extend_from_slice(&page_num.to_le_bytes());
            buf.extend_from_slice(&checksum.to_le_bytes());
            buf.extend_from_slice(&length.to_le_bytes());
        } else {
            buf.push(0);
        }

        // System root
        if let Some((page_num, checksum, length)) = self.system_root {
            buf.push(1);
            buf.extend_from_slice(&page_num.to_le_bytes());
            buf.extend_from_slice(&checksum.to_le_bytes());
            buf.extend_from_slice(&length.to_le_bytes());
        } else {
            buf.push(0);
        }

        // Freed pages
        #[allow(clippy::cast_possible_truncation)]
        buf.extend_from_slice(&(self.freed_pages.len() as u32).to_le_bytes());
        for page_num in &self.freed_pages {
            buf.extend_from_slice(&page_num.to_le_bytes());
        }

        // Allocated pages
        #[allow(clippy::cast_possible_truncation)]
        buf.extend_from_slice(&(self.allocated_pages.len() as u32).to_le_bytes());
        for page_num in &self.allocated_pages {
            buf.extend_from_slice(&page_num.to_le_bytes());
        }

        // Durability
        let durability_byte = match self.durability {
            Durability::None => 0,
            Durability::Immediate => 1,
        };
        buf.push(durability_byte);
    }

    /// Deserializes the payload from bytes.
    ///
    /// Returns the payload and the number of bytes consumed.
    fn deserialize_from(data: &[u8]) -> io::Result<(Self, usize)> {
        let mut offset = 0;

        // User root
        if data.len() < offset + 1 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated user_root flag",
            ));
        }
        let user_root = if data[offset] == 1 {
            offset += 1;
            if data.len() < offset + 8 + 16 + 8 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated user_root",
                ));
            }
            let page_num = PageNumber::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
            offset += 8;
            let checksum = u128::from_le_bytes(data[offset..offset + 16].try_into().unwrap());
            offset += 16;
            let length = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
            offset += 8;
            Some((page_num, checksum, length))
        } else {
            offset += 1;
            None
        };

        // System root
        if data.len() < offset + 1 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated system_root flag",
            ));
        }
        let system_root = if data[offset] == 1 {
            offset += 1;
            if data.len() < offset + 8 + 16 + 8 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated system_root",
                ));
            }
            let page_num = PageNumber::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
            offset += 8;
            let checksum = u128::from_le_bytes(data[offset..offset + 16].try_into().unwrap());
            offset += 16;
            let length = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
            offset += 8;
            Some((page_num, checksum, length))
        } else {
            offset += 1;
            None
        };

        // Freed pages
        if data.len() < offset + 4 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated freed_pages count",
            ));
        }
        let freed_pages_count =
            u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;

        let mut freed_pages = Vec::with_capacity(freed_pages_count);
        for _ in 0..freed_pages_count {
            if data.len() < offset + 8 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated freed_page",
                ));
            }
            let page_num = PageNumber::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
            offset += 8;
            freed_pages.push(page_num);
        }

        // Allocated pages
        if data.len() < offset + 4 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated allocated_pages count",
            ));
        }
        let allocated_pages_count =
            u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;

        let mut allocated_pages = Vec::with_capacity(allocated_pages_count);
        for _ in 0..allocated_pages_count {
            if data.len() < offset + 8 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated allocated_page",
                ));
            }
            let page_num = PageNumber::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
            offset += 8;
            allocated_pages.push(page_num);
        }

        // Durability
        if data.len() < offset + 1 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated durability",
            ));
        }
        let durability = match data[offset] {
            0 => Durability::None,
            1 => Durability::Immediate,
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid durability value: {other}"),
                ));
            }
        };
        offset += 1;

        Ok((
            Self {
                user_root,
                system_root,
                freed_pages,
                allocated_pages,
                durability,
            },
            offset,
        ))
    }
}

impl WALLogicalOpsPayload {
    /// Serializes the logical ops payload into the given buffer.
    ///
    /// Format:
    /// - `name_len`: u16 (2 bytes)
    /// - `name_bytes`: [u8; `name_len`] (variable)
    /// - `op_count`: u32 (4 bytes)
    /// - for each op:
    ///   - `key_len`: u32 (4 bytes)
    ///   - `key`: [u8; `key_len`] (variable)
    ///   - `has_value`: u8 (0 = delete, 1 = insert/update)
    ///   - if `has_value`:
    ///     - `value_len`: u32 (4 bytes)
    ///     - `value`: [u8; `value_len`] (variable)
    fn serialize_into(&self, buf: &mut Vec<u8>) {
        // Table name
        let name_bytes = self.table_name.as_bytes();
        #[allow(clippy::cast_possible_truncation)]
        buf.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        buf.extend_from_slice(name_bytes);

        // Ops
        #[allow(clippy::cast_possible_truncation)]
        buf.extend_from_slice(&(self.ops.len() as u32).to_le_bytes());
        for op in &self.ops {
            #[allow(clippy::cast_possible_truncation)]
            buf.extend_from_slice(&(op.key.len() as u32).to_le_bytes());
            buf.extend_from_slice(&op.key);

            if let Some(value) = &op.value {
                buf.push(1);
                #[allow(clippy::cast_possible_truncation)]
                buf.extend_from_slice(&(value.len() as u32).to_le_bytes());
                buf.extend_from_slice(value);
            } else {
                buf.push(0);
            }
        }
    }

    /// Deserializes the logical ops payload from bytes.
    ///
    /// Returns the payload and the number of bytes consumed.
    fn deserialize_from(data: &[u8]) -> io::Result<(Self, usize)> {
        let mut offset = 0;

        // Table name
        if data.len() < offset + 2 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated table_name length",
            ));
        }
        let name_len = u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap()) as usize;
        offset += 2;

        if data.len() < offset + name_len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated table_name",
            ));
        }
        let table_name =
            String::from_utf8(data[offset..offset + name_len].to_vec()).map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidData, format!("invalid UTF-8: {e}"))
            })?;
        offset += name_len;

        // Op count
        if data.len() < offset + 4 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated op_count",
            ));
        }
        let op_count = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;

        let mut ops = Vec::with_capacity(op_count);
        for _ in 0..op_count {
            // Key
            if data.len() < offset + 4 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated key_len",
                ));
            }
            let key_len = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
            offset += 4;

            if data.len() < offset + key_len {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated key",
                ));
            }
            let key = data[offset..offset + key_len].to_vec();
            offset += key_len;

            // has_value
            if data.len() < offset + 1 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated has_value",
                ));
            }
            let has_value = data[offset];
            offset += 1;

            let value = if has_value == 1 {
                if data.len() < offset + 4 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "truncated value_len",
                    ));
                }
                let value_len =
                    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
                offset += 4;

                if data.len() < offset + value_len {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "truncated value",
                    ));
                }
                let v = data[offset..offset + value_len].to_vec();
                offset += value_len;
                Some(v)
            } else {
                None
            };

            ops.push(WALOp { key, value });
        }

        Ok((Self { table_name, ops }, offset))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_entry_serialization_round_trip() {
        let payload = WALTransactionPayload {
            user_root: Some((PageNumber::new(0, 1, 0), 0x1234567890abcdef, 100)),
            system_root: None,
            freed_pages: vec![PageNumber::new(0, 2, 0), PageNumber::new(0, 3, 0)],
            allocated_pages: vec![PageNumber::new(0, 4, 0)],
            durability: Durability::Immediate,
        };

        let entry = WALEntry {
            sequence: 42,
            cf_name: "test_cf".to_string(),
            transaction_id: 100,
            payload: WALPayload::Transaction(payload),
        };

        let bytes = entry.to_bytes();
        let (decoded, len) = WALEntry::from_bytes(&bytes).unwrap();

        assert_eq!(len, bytes.len());
        assert_eq!(decoded.sequence, entry.sequence);
        assert_eq!(decoded.cf_name, entry.cf_name);
        assert_eq!(decoded.transaction_id, entry.transaction_id);
        assert_eq!(decoded.payload, entry.payload);
    }

    #[test]
    fn test_payload_serialization_round_trip() {
        let payload = WALTransactionPayload {
            user_root: Some((PageNumber::new(1, 10, 2), 0xdeadbeef, 200)),
            system_root: Some((PageNumber::new(2, 20, 1), 0xcafebabe, 300)),
            freed_pages: vec![],
            allocated_pages: vec![
                PageNumber::new(0, 5, 0),
                PageNumber::new(0, 6, 0),
                PageNumber::new(0, 7, 0),
            ],
            durability: Durability::None,
        };

        let mut buf = Vec::new();
        payload.serialize_into(&mut buf);

        let (decoded, len) = WALTransactionPayload::deserialize_from(&buf).unwrap();

        assert_eq!(len, buf.len());
        assert_eq!(decoded, payload);
    }

    #[test]
    fn test_empty_payload() {
        let payload = WALTransactionPayload {
            user_root: None,
            system_root: None,
            freed_pages: vec![],
            allocated_pages: vec![],
            durability: Durability::Immediate,
        };

        let mut buf = Vec::new();
        payload.serialize_into(&mut buf);

        let (decoded, _) = WALTransactionPayload::deserialize_from(&buf).unwrap();

        assert_eq!(decoded, payload);
    }

    #[test]
    fn test_logical_ops_serialization_round_trip() {
        let ops_payload = WALLogicalOpsPayload {
            table_name: "my_table".to_string(),
            ops: vec![
                WALOp {
                    key: b"key1".to_vec(),
                    value: Some(b"value1".to_vec()),
                },
                WALOp {
                    key: b"key2".to_vec(),
                    value: None, // delete
                },
                WALOp {
                    key: b"key3".to_vec(),
                    value: Some(vec![]),
                },
            ],
        };

        let mut buf = Vec::new();
        ops_payload.serialize_into(&mut buf);

        let (decoded, len) = WALLogicalOpsPayload::deserialize_from(&buf).unwrap();
        assert_eq!(len, buf.len());
        assert_eq!(decoded, ops_payload);
    }

    #[test]
    fn test_logical_ops_entry_round_trip() {
        let entry = WALEntry::new_logical_ops(
            "test_cf".to_string(),
            42,
            "my_table".to_string(),
            vec![
                WALOp {
                    key: b"hello".to_vec(),
                    value: Some(b"world".to_vec()),
                },
                WALOp {
                    key: b"delete_me".to_vec(),
                    value: None,
                },
            ],
        );

        let bytes = entry.to_bytes();
        let (decoded, len) = WALEntry::from_bytes(&bytes).unwrap();

        assert_eq!(len, bytes.len());
        assert_eq!(decoded.sequence, entry.sequence);
        assert_eq!(decoded.cf_name, entry.cf_name);
        assert_eq!(decoded.transaction_id, entry.transaction_id);
        assert_eq!(decoded.payload, entry.payload);
    }
}
