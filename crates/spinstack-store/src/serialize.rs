use crate::error::{StoreError, Result};

pub fn encode_u64(buf: &mut Vec<u8>, val: u64) { buf.extend_from_slice(&val.to_le_bytes()); }
pub fn decode_u64(buf: &[u8]) -> Result<(u64, usize)> {
    if buf.len() < 8 { return Err(StoreError::SerializationError("truncated u64".into())); }
    Ok((u64::from_le_bytes(buf[..8].try_into().unwrap()), 8))
}
pub fn encode_i64(buf: &mut Vec<u8>, val: i64) { buf.extend_from_slice(&val.to_le_bytes()); }
pub fn decode_i64(buf: &[u8]) -> Result<(i64, usize)> {
    if buf.len() < 8 { return Err(StoreError::SerializationError("truncated i64".into())); }
    Ok((i64::from_le_bytes(buf[..8].try_into().unwrap()), 8))
}
pub fn encode_f64(buf: &mut Vec<u8>, val: f64) { buf.extend_from_slice(&val.to_le_bytes()); }
pub fn decode_f64(buf: &[u8]) -> Result<(f64, usize)> {
    if buf.len() < 8 { return Err(StoreError::SerializationError("truncated f64".into())); }
    Ok((f64::from_le_bytes(buf[..8].try_into().unwrap()), 8))
}
pub fn encode_bool(buf: &mut Vec<u8>, val: bool) { buf.push(if val { 1 } else { 0 }); }
pub fn decode_bool(buf: &[u8]) -> Result<(bool, usize)> {
    if buf.is_empty() { return Err(StoreError::SerializationError("truncated bool".into())); }
    Ok((buf[0] != 0, 1))
}
pub fn encode_string(buf: &mut Vec<u8>, val: &str) {
    let bytes = val.as_bytes();
    buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(bytes);
}
pub fn decode_string(buf: &[u8]) -> Result<(String, usize)> {
    if buf.len() < 4 { return Err(StoreError::SerializationError("truncated string length".into())); }
    let len = u32::from_le_bytes(buf[..4].try_into().unwrap()) as usize;
    if buf.len() < 4 + len { return Err(StoreError::SerializationError("truncated string data".into())); }
    let s = String::from_utf8(buf[4..4 + len].to_vec())
        .map_err(|e| StoreError::SerializationError(format!("invalid utf8: {e}")))?;
    Ok((s, 4 + len))
}
pub fn encode_bytes(buf: &mut Vec<u8>, val: &[u8]) {
    buf.extend_from_slice(&(val.len() as u32).to_le_bytes());
    buf.extend_from_slice(val);
}
pub fn decode_bytes(buf: &[u8]) -> Result<(Vec<u8>, usize)> {
    if buf.len() < 4 { return Err(StoreError::SerializationError("truncated bytes length".into())); }
    let len = u32::from_le_bytes(buf[..4].try_into().unwrap()) as usize;
    if buf.len() < 4 + len { return Err(StoreError::SerializationError("truncated bytes data".into())); }
    Ok((buf[4..4 + len].to_vec(), 4 + len))
}
