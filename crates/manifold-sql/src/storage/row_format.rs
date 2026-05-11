//! Binary row encoding and decoding for storing SQL rows in Manifold tables.
//!
//! Layout:
//! ```text
//! [column_count: u16]
//! [null_bitmap: ceil(col_count/8) bytes]
//! [fixed-width values: in column order, null slots zeroed]
//! [var-width offsets: u32 per variable-width column, each storing END offset into var data]
//! [var-width data: concatenated]
//! ```

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use uuid::Uuid;

use crate::error::{Result, SqlError};
use crate::types::{SqlType, Value};

/// Encode a row of values into the binary row format.
pub fn encode_row(types: &[SqlType], values: &[Value]) -> Result<Vec<u8>> {
    if types.len() != values.len() {
        return Err(SqlError::Internal(format!(
            "column count mismatch: {} types vs {} values",
            types.len(),
            values.len()
        )));
    }

    let col_count = types.len();
    let bitmap_len = col_count.div_ceil(8);

    // Build null bitmap.
    let mut bitmap = vec![0u8; bitmap_len];
    for (i, val) in values.iter().enumerate() {
        if val.is_null() {
            bitmap[i / 8] |= 1 << (i % 8);
        }
    }

    let mut buf = Vec::new();

    // Column count.
    buf.extend_from_slice(&(col_count as u16).to_le_bytes());

    // Null bitmap.
    buf.extend_from_slice(&bitmap);

    // Fixed-width values (null slots are zeroed).
    for (i, ty) in types.iter().enumerate() {
        if let Some(width) = ty.fixed_width() {
            if values[i].is_null() {
                buf.extend(std::iter::repeat_n(0u8, width));
            } else {
                encode_fixed_value(&values[i], ty, &mut buf)?;
            }
        }
    }

    // Collect variable-width data.
    let mut var_data = Vec::new();
    let mut var_offsets = Vec::new();

    for (i, ty) in types.iter().enumerate() {
        if ty.is_variable_width() {
            if values[i].is_null() {
                // Zero-length entry: offset equals current length.
                var_offsets.push(var_data.len() as u32);
            } else {
                encode_var_value(&values[i], &mut var_data)?;
                var_offsets.push(var_data.len() as u32);
            }
        }
    }

    // Write var-width end offsets.
    for off in &var_offsets {
        buf.extend_from_slice(&off.to_le_bytes());
    }

    // Write var-width data.
    buf.extend_from_slice(&var_data);

    Ok(buf)
}

/// Decode all columns from a binary-encoded row.
pub fn decode_row(types: &[SqlType], data: &[u8]) -> Result<Vec<Value>> {
    if data.len() < 2 {
        return Err(SqlError::Internal("row data too short".into()));
    }

    let col_count = u16::from_le_bytes([data[0], data[1]]) as usize;
    if col_count != types.len() {
        return Err(SqlError::Internal(format!(
            "column count mismatch: header says {col_count} but {} types provided",
            types.len()
        )));
    }

    let mut values = Vec::with_capacity(col_count);
    for i in 0..col_count {
        values.push(decode_column(types, data, i)?);
    }
    Ok(values)
}

/// Decode a single column from a binary-encoded row. O(1) access for fixed-width columns.
pub fn decode_column(types: &[SqlType], data: &[u8], col_index: usize) -> Result<Value> {
    if data.len() < 2 {
        return Err(SqlError::Internal("row data too short".into()));
    }

    let col_count = u16::from_le_bytes([data[0], data[1]]) as usize;
    if col_index >= col_count {
        return Err(SqlError::Internal(format!(
            "column index {col_index} out of range (row has {col_count} columns)"
        )));
    }
    if col_count != types.len() {
        return Err(SqlError::Internal(format!(
            "column count mismatch: header says {col_count} but {} types provided",
            types.len()
        )));
    }

    let bitmap_len = col_count.div_ceil(8);

    // Check null bitmap.
    let bitmap_byte = data[2 + col_index / 8];
    let is_null = (bitmap_byte >> (col_index % 8)) & 1 == 1;

    if is_null {
        return Ok(Value::Null);
    }

    // Compute the offset of the fixed-width region.
    let fixed_region_start = 2 + bitmap_len;

    // Compute the byte offset of this column within the fixed-width region.
    let ty = &types[col_index];

    if let Some(_width) = ty.fixed_width() {
        // Sum up fixed widths of columns before col_index.
        let mut offset = fixed_region_start;
        for t in &types[..col_index] {
            if let Some(w) = t.fixed_width() {
                offset += w;
            }
        }
        decode_fixed_value(ty, &data[offset..])
    } else {
        // Variable-width column.
        // Count how many variable-width columns are before this one,
        // and compute offsets into the var-width offset table and data.
        let var_col_index = types[..col_index]
            .iter()
            .filter(|t| t.is_variable_width())
            .count();

        // Fixed region total size.
        let fixed_total: usize = types
            .iter()
            .filter_map(|t| t.fixed_width())
            .sum();

        let var_col_count = types.iter().filter(|t| t.is_variable_width()).count();

        let var_offsets_start = fixed_region_start + fixed_total;
        let var_data_start = var_offsets_start + var_col_count * 4;

        // Read the end offset for this var column.
        let offset_pos = var_offsets_start + var_col_index * 4;
        let end_offset =
            u32::from_le_bytes(data[offset_pos..offset_pos + 4].try_into().unwrap()) as usize;

        // Read the start offset (previous column's end, or 0 for first var column).
        let start_offset = if var_col_index == 0 {
            0
        } else {
            let prev_pos = var_offsets_start + (var_col_index - 1) * 4;
            u32::from_le_bytes(data[prev_pos..prev_pos + 4].try_into().unwrap()) as usize
        };

        let var_bytes = &data[var_data_start + start_offset..var_data_start + end_offset];
        decode_var_value(ty, var_bytes)
    }
}

fn encode_fixed_value(value: &Value, _ty: &SqlType, buf: &mut Vec<u8>) -> Result<()> {
    match value {
        Value::Boolean(v) => buf.push(if *v { 1 } else { 0 }),
        Value::SmallInt(v) => buf.extend_from_slice(&v.to_le_bytes()),
        Value::Integer(v) => buf.extend_from_slice(&v.to_le_bytes()),
        Value::Real(v) => buf.extend_from_slice(&v.to_le_bytes()),
        Value::Decimal(v) => buf.extend_from_slice(&v.serialize()),
        Value::Uuid(v) => buf.extend_from_slice(v.as_bytes()),
        Value::Date(v) => {
            let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
            let days = (*v - epoch).num_days() as i32;
            buf.extend_from_slice(&days.to_le_bytes());
        }
        Value::Timestamp(v) => {
            let epoch = DateTime::<Utc>::UNIX_EPOCH.naive_utc();
            let micros = (*v - epoch)
                .num_microseconds()
                .ok_or_else(|| SqlError::Internal("timestamp out of range".into()))?;
            buf.extend_from_slice(&micros.to_le_bytes());
        }
        Value::TimestampTz(v) => {
            let micros = v
                .signed_duration_since(DateTime::<Utc>::UNIX_EPOCH)
                .num_microseconds()
                .ok_or_else(|| SqlError::Internal("timestamptz out of range".into()))?;
            buf.extend_from_slice(&micros.to_le_bytes());
        }
        _ => {
            return Err(SqlError::Internal(format!(
                "cannot encode {value:?} as fixed-width"
            )));
        }
    }
    Ok(())
}

fn encode_var_value(value: &Value, buf: &mut Vec<u8>) -> Result<()> {
    match value {
        Value::Text(s) => buf.extend_from_slice(s.as_bytes()),
        Value::Blob(b) => buf.extend_from_slice(b),
        Value::Json(v) => {
            let s = serde_json::to_string(v)
                .map_err(|e| SqlError::Internal(format!("JSON serialization error: {e}")))?;
            buf.extend_from_slice(s.as_bytes());
        }
        _ => {
            return Err(SqlError::Internal(format!(
                "cannot encode {value:?} as variable-width"
            )));
        }
    }
    Ok(())
}

fn decode_fixed_value(ty: &SqlType, data: &[u8]) -> Result<Value> {
    match ty {
        SqlType::Boolean => Ok(Value::Boolean(data[0] != 0)),
        SqlType::SmallInt => {
            let v = i16::from_le_bytes(data[..2].try_into().unwrap());
            Ok(Value::SmallInt(v))
        }
        SqlType::Integer | SqlType::BigInt => {
            let v = i64::from_le_bytes(data[..8].try_into().unwrap());
            Ok(Value::Integer(v))
        }
        SqlType::Real => {
            let v = f64::from_le_bytes(data[..8].try_into().unwrap());
            Ok(Value::Real(v))
        }
        SqlType::Decimal { .. } => {
            let bytes: [u8; 16] = data[..16].try_into().unwrap();
            Ok(Value::Decimal(Decimal::deserialize(bytes)))
        }
        SqlType::Uuid => {
            let bytes: [u8; 16] = data[..16].try_into().unwrap();
            Ok(Value::Uuid(Uuid::from_bytes(bytes)))
        }
        SqlType::Date => {
            let days = i32::from_le_bytes(data[..4].try_into().unwrap());
            let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
            let date = epoch
                + chrono::Duration::days(days as i64);
            Ok(Value::Date(date))
        }
        SqlType::Timestamp => {
            let micros = i64::from_le_bytes(data[..8].try_into().unwrap());
            let ts = DateTime::<Utc>::UNIX_EPOCH.naive_utc()
                + chrono::Duration::microseconds(micros);
            Ok(Value::Timestamp(ts))
        }
        SqlType::TimestampTz => {
            let micros = i64::from_le_bytes(data[..8].try_into().unwrap());
            let ts = DateTime::<Utc>::UNIX_EPOCH
                + chrono::Duration::microseconds(micros);
            Ok(Value::TimestampTz(ts))
        }
        _ => Err(SqlError::Internal(format!(
            "type {ty} is not fixed-width"
        ))),
    }
}

fn decode_var_value(ty: &SqlType, data: &[u8]) -> Result<Value> {
    match ty {
        SqlType::Text | SqlType::Varchar(_) => {
            let s = std::str::from_utf8(data)
                .map_err(|e| SqlError::Internal(format!("invalid UTF-8: {e}")))?;
            Ok(Value::Text(s.to_owned()))
        }
        SqlType::Blob => Ok(Value::Blob(data.to_vec())),
        SqlType::Json => {
            let s = std::str::from_utf8(data)
                .map_err(|e| SqlError::Internal(format!("invalid UTF-8 in JSON: {e}")))?;
            let v: serde_json::Value = serde_json::from_str(s)
                .map_err(|e| SqlError::Internal(format!("invalid JSON: {e}")))?;
            Ok(Value::Json(v))
        }
        _ => Err(SqlError::Internal(format!(
            "type {ty} is not variable-width"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn encode_decode_fixed_width_only() {
        let types = vec![SqlType::Integer, SqlType::Boolean, SqlType::SmallInt];
        let values = vec![
            Value::Integer(42),
            Value::Boolean(true),
            Value::SmallInt(-7),
        ];

        let encoded = encode_row(&types, &values).unwrap();
        let decoded = decode_row(&types, &encoded).unwrap();
        assert_eq!(values, decoded);
    }

    #[test]
    fn encode_decode_variable_width() {
        let types = vec![SqlType::Integer, SqlType::Text, SqlType::Blob];
        let values = vec![
            Value::Integer(100),
            Value::Text("hello world".into()),
            Value::Blob(vec![0xDE, 0xAD, 0xBE, 0xEF]),
        ];

        let encoded = encode_row(&types, &values).unwrap();
        let decoded = decode_row(&types, &encoded).unwrap();
        assert_eq!(values, decoded);
    }

    #[test]
    fn encode_decode_with_nulls() {
        let types = vec![SqlType::Integer, SqlType::Text, SqlType::Boolean];
        let values = vec![Value::Null, Value::Text("present".into()), Value::Null];

        let encoded = encode_row(&types, &values).unwrap();
        let decoded = decode_row(&types, &encoded).unwrap();
        assert_eq!(values, decoded);
    }

    #[test]
    fn decode_single_column() {
        let types = vec![SqlType::Integer, SqlType::Text, SqlType::Boolean];
        let values = vec![
            Value::Integer(999),
            Value::Text("target".into()),
            Value::Boolean(false),
        ];

        let encoded = encode_row(&types, &values).unwrap();

        // Decode just column 1 (the Text column).
        let val = decode_column(&types, &encoded, 1).unwrap();
        assert_eq!(val, Value::Text("target".into()));

        // Verify other columns independently.
        assert_eq!(
            decode_column(&types, &encoded, 0).unwrap(),
            Value::Integer(999)
        );
        assert_eq!(
            decode_column(&types, &encoded, 2).unwrap(),
            Value::Boolean(false)
        );
    }

    #[test]
    fn encode_decode_all_types() {
        let types = vec![
            SqlType::Boolean,
            SqlType::SmallInt,
            SqlType::Integer,
            SqlType::BigInt,
            SqlType::Real,
            SqlType::Decimal {
                precision: 10,
                scale: 2,
            },
            SqlType::Text,
            SqlType::Varchar(255),
            SqlType::Blob,
            SqlType::Uuid,
            SqlType::Date,
            SqlType::Timestamp,
            SqlType::TimestampTz,
            SqlType::Json,
        ];

        let test_uuid = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let test_date = NaiveDate::from_ymd_opt(2025, 6, 15).unwrap();
        let test_ts = NaiveDate::from_ymd_opt(2025, 6, 15)
            .unwrap()
            .and_hms_opt(12, 30, 45)
            .unwrap();
        let test_tstz = Utc.with_ymd_and_hms(2025, 6, 15, 12, 30, 45).unwrap();
        let test_json: serde_json::Value =
            serde_json::json!({"key": "value", "num": 42});
        let test_decimal = Decimal::new(12345, 2); // 123.45

        let values = vec![
            Value::Boolean(true),
            Value::SmallInt(32000),
            Value::Integer(1_000_000),
            Value::Integer(9_000_000_000),
            Value::Real(3.14159),
            Value::Decimal(test_decimal),
            Value::Text("hello".into()),
            Value::Text("varchar value".into()),
            Value::Blob(vec![1, 2, 3, 4, 5]),
            Value::Uuid(test_uuid),
            Value::Date(test_date),
            Value::Timestamp(test_ts),
            Value::TimestampTz(test_tstz),
            Value::Json(test_json),
        ];

        let encoded = encode_row(&types, &values).unwrap();
        let decoded = decode_row(&types, &encoded).unwrap();
        assert_eq!(values, decoded);
    }

    #[test]
    fn empty_row() {
        let types: Vec<SqlType> = vec![];
        let values: Vec<Value> = vec![];

        let encoded = encode_row(&types, &values).unwrap();
        assert_eq!(encoded, vec![0, 0]); // Just the u16 column count = 0.
        let decoded = decode_row(&types, &encoded).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn wrong_column_count_errors() {
        let types = vec![SqlType::Integer, SqlType::Text];
        let values = vec![Value::Integer(1)];

        let result = encode_row(&types, &values);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("mismatch"),
            "error should mention mismatch: {err_msg}"
        );
    }
}
