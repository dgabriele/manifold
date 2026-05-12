use spinstack_store::*;
use spinstack_store::schema::StoreRecord;

#[derive(Store)]
struct Post {
    #[primary_key]
    id: u64,
    #[indexed]
    author_id: u64,
    content: String,
    created_at: i64,
}

#[test]
fn test_table_name() {
    assert_eq!(Post::TABLE_NAME, "post");
}

#[test]
fn test_schema() {
    let schema = Post::schema();
    assert_eq!(schema.table_name, "post");
    assert_eq!(schema.columns.len(), 4);
    assert!(schema.columns[0].is_primary_key);
    assert_eq!(schema.columns[0].name, "id");
    assert!(schema.columns[1].is_indexed);
    assert_eq!(schema.columns[1].name, "author_id");
}

#[test]
fn test_field_accessors() {
    assert_eq!(Post::ID.column_index, 0);
    assert_eq!(Post::AUTHOR_ID.column_index, 1);
    assert_eq!(Post::CONTENT.column_index, 2);
    assert_eq!(Post::CREATED_AT.column_index, 3);
}

#[test]
fn test_serialization_round_trip() {
    let post = Post { id: 42, author_id: 7, content: "hello world".to_string(), created_at: 1234567890 };
    let bytes = post.to_store_bytes();
    let decoded = Post::from_store_bytes(&bytes).unwrap();
    assert_eq!(decoded.id, 42);
    assert_eq!(decoded.author_id, 7);
    assert_eq!(decoded.content, "hello world");
    assert_eq!(decoded.created_at, 1234567890);
}

#[test]
fn test_primary_key_bytes() {
    let post = Post { id: 42, author_id: 7, content: "test".to_string(), created_at: 0 };
    let key_bytes = post.primary_key_bytes();
    assert_eq!(key_bytes, 42u64.to_le_bytes().to_vec());
}

#[test]
fn test_filter_with_field() {
    let filter = Post::AUTHOR_ID.eq(42u64);
    assert_eq!(filter.column_index, 1);
    assert_eq!(filter.op, FilterOp::Eq);
    assert_eq!(filter.value, Value::U64(42));
}
