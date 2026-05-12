use spinstack_store::serialize::*;

#[test]
fn test_u64_round_trip() {
    let mut buf = Vec::new();
    encode_u64(&mut buf, 42);
    let (val, consumed) = decode_u64(&buf).unwrap();
    assert_eq!(val, 42);
    assert_eq!(consumed, 8);
}

#[test]
fn test_string_round_trip() {
    let mut buf = Vec::new();
    encode_string(&mut buf, "hello world");
    let (val, consumed) = decode_string(&buf).unwrap();
    assert_eq!(val, "hello world");
    assert_eq!(consumed, 4 + 11);
}

#[test]
fn test_mixed_row() {
    let mut buf = Vec::new();
    encode_u64(&mut buf, 1);
    encode_u64(&mut buf, 99);
    encode_string(&mut buf, "test post");
    encode_i64(&mut buf, 1234567890);

    let mut offset = 0;
    let (id, n) = decode_u64(&buf[offset..]).unwrap(); offset += n;
    let (author, n) = decode_u64(&buf[offset..]).unwrap(); offset += n;
    let (content, n) = decode_string(&buf[offset..]).unwrap(); offset += n;
    let (ts, _) = decode_i64(&buf[offset..]).unwrap();

    assert_eq!(id, 1);
    assert_eq!(author, 99);
    assert_eq!(content, "test post");
    assert_eq!(ts, 1234567890);
}
