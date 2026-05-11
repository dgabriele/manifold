//! Order-preserving index key encoding for Manifold.
//!
//! Manifold compares keys byte-by-byte, so all encodings must produce bytes
//! where lexicographic byte order matches logical value order.

use chrono::Datelike;

use crate::types::Value;

/// Encode a slice of values into a sort-order-preserving composite key.
///
/// Each value is encoded such that lexicographic byte comparison of the
/// resulting keys matches the logical comparison of the original values.
pub fn encode_index_key(values: &[Value]) -> Vec<u8> {
    let mut buf = Vec::new();
    for value in values {
        encode_value(value, &mut buf);
    }
    buf
}

/// Encode a prefix key for prefix range scans.
///
/// Equivalent to `encode_index_key` — the same encoding is used for both
/// full keys and prefixes, since any full key that starts with the encoded
/// prefix values will byte-wise start with the prefix bytes.
pub fn encode_index_key_prefix(values: &[Value]) -> Vec<u8> {
    encode_index_key(values)
}

fn encode_value(value: &Value, buf: &mut Vec<u8>) {
    match value {
        Value::Null => {
            buf.push(0x00); // NULL tag — sorts before all non-NULL values
        }
        Value::Boolean(b) => {
            buf.push(0x01); // non-NULL tag
            buf.push(if *b { 1 } else { 0 });
        }
        Value::SmallInt(v) => {
            buf.push(0x01);
            // Flip the sign bit so that negative numbers sort before positive.
            let encoded = (*v as u16) ^ 0x8000u16;
            buf.extend_from_slice(&encoded.to_be_bytes());
        }
        Value::Integer(v) => {
            buf.push(0x01);
            // Flip the sign bit so that negative numbers sort before positive.
            let encoded = (*v as u64) ^ 0x8000_0000_0000_0000u64;
            buf.extend_from_slice(&encoded.to_be_bytes());
        }
        Value::Real(v) => {
            buf.push(0x01);
            let bits = v.to_bits();
            // IEEE 754 order-preserving transform:
            //   - negative: flip all bits
            //   - positive (or +0): flip only the sign bit
            let encoded = if *v < 0.0 {
                !bits
            } else {
                bits ^ 0x8000_0000_0000_0000u64
            };
            buf.extend_from_slice(&encoded.to_be_bytes());
        }
        Value::Decimal(d) => {
            buf.push(0x01);
            // rust_decimal::Decimal::serialize() returns [u8; 16].
            buf.extend_from_slice(&d.serialize());
        }
        Value::Text(s) => {
            buf.push(0x01);
            // Use null-terminated, null-escaped encoding so that byte order
            // matches lexicographic string order in composite keys:
            //   0x00 in content is escaped as 0x00 0xFF
            //   string is terminated with 0x00 0x00
            encode_bytes_escaped(s.as_bytes(), buf);
        }
        Value::Blob(b) => {
            buf.push(0x01);
            // Same null-escaped encoding as Text for order preservation.
            encode_bytes_escaped(b, buf);
        }
        Value::Uuid(u) => {
            buf.push(0x01);
            buf.extend_from_slice(u.as_bytes());
        }
        Value::Date(d) => {
            buf.push(0x01);
            // NaiveDate::num_days_from_ce() returns an i32 days count.
            // Flip sign bit for order-preserving big-endian encoding.
            let days = d.num_days_from_ce();
            let encoded = (days as u32) ^ 0x8000_0000u32;
            buf.extend_from_slice(&encoded.to_be_bytes());
        }
        Value::Timestamp(dt) => {
            buf.push(0x01);
            // Encode as i64 microseconds from the Unix epoch, sign-bit flipped.
            let micros = dt.and_utc().timestamp_micros();
            let encoded = (micros as u64) ^ 0x8000_0000_0000_0000u64;
            buf.extend_from_slice(&encoded.to_be_bytes());
        }
        Value::TimestampTz(dt) => {
            buf.push(0x01);
            let micros = dt.timestamp_micros();
            let encoded = (micros as u64) ^ 0x8000_0000_0000_0000u64;
            buf.extend_from_slice(&encoded.to_be_bytes());
        }
        Value::Json(j) => {
            buf.push(0x01);
            let text = j.to_string();
            encode_bytes_escaped(text.as_bytes(), buf);
        }
    }
}

/// Encode a byte slice in a null-terminated, null-escaped form so that
/// lexicographic byte comparison of the result matches the original byte order.
///
/// Encoding:
///   - Each 0x00 byte in `src` is replaced with the two-byte sequence 0x00 0xFF.
///   - The output is terminated with the two-byte sequence 0x00 0x00.
///
/// This ensures that strings which are a prefix of another sort before it,
/// and that the bytes of the string content drive ordering correctly.
fn encode_bytes_escaped(src: &[u8], buf: &mut Vec<u8>) {
    for &byte in src {
        if byte == 0x00 {
            buf.push(0x00);
            buf.push(0xFF);
        } else {
            buf.push(byte);
        }
    }
    // Null terminator
    buf.push(0x00);
    buf.push(0x00);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_ordering_preserved() {
        let neg = encode_index_key(&[Value::Integer(-1)]);
        let one = encode_index_key(&[Value::Integer(1)]);
        let two = encode_index_key(&[Value::Integer(2)]);

        assert!(neg < one, "encode(-1) should be less than encode(1)");
        assert!(one < two, "encode(1) should be less than encode(2)");
    }

    #[test]
    fn text_ordering_preserved() {
        let alice = encode_index_key(&[Value::Text("alice".to_string())]);
        let bob = encode_index_key(&[Value::Text("bob".to_string())]);

        assert!(alice < bob, "encode(\"alice\") should be less than encode(\"bob\")");
    }

    #[test]
    fn composite_key_ordering() {
        let k1a = encode_index_key(&[Value::Integer(1), Value::Text("a".to_string())]);
        let k1b = encode_index_key(&[Value::Integer(1), Value::Text("b".to_string())]);
        let k2a = encode_index_key(&[Value::Integer(2), Value::Text("a".to_string())]);

        assert!(k1a < k1b, "(1,\"a\") should be less than (1,\"b\")");
        assert!(k1b < k2a, "(1,\"b\") should be less than (2,\"a\")");
        assert!(k1a < k2a, "(1,\"a\") should be less than (2,\"a\")");
    }

    #[test]
    fn null_sorts_first() {
        let null_key = encode_index_key(&[Value::Null]);
        let zero_key = encode_index_key(&[Value::Integer(0)]);

        assert!(null_key < zero_key, "NULL should sort before Integer(0)");
    }

    #[test]
    fn prefix_scan_works() {
        let prefix = encode_index_key_prefix(&[Value::Integer(1)]);

        let k1a = encode_index_key(&[Value::Integer(1), Value::Text("a".to_string())]);
        let k1b = encode_index_key(&[Value::Integer(1), Value::Text("b".to_string())]);
        let k2a = encode_index_key(&[Value::Integer(2), Value::Text("a".to_string())]);

        assert!(
            k1a.starts_with(&prefix),
            "(1,\"a\") key should start with the prefix for Integer(1)"
        );
        assert!(
            k1b.starts_with(&prefix),
            "(1,\"b\") key should start with the prefix for Integer(1)"
        );
        assert!(
            !k2a.starts_with(&prefix),
            "(2,\"a\") key should NOT start with the prefix for Integer(1)"
        );
    }
}
