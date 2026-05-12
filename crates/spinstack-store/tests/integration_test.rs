use spinstack_store::*;
use spinstack_store::descriptor::QueryDescriptor;
use spinstack_store::schema::StoreRecord;

#[derive(Store, Debug, PartialEq)]
struct User {
    #[primary_key]
    id: u64,
    username: String,
    active: bool,
}

#[derive(Store, Debug, PartialEq)]
struct Comment {
    #[primary_key]
    id: u64,
    #[indexed]
    post_id: u64,
    author_id: u64,
    body: String,
}

#[test]
fn test_query_with_derived_struct() {
    let desc = Query::<Comment>::new(Comment::TABLE_NAME)
        .filter(Comment::POST_ID.eq(42u64))
        .limit(10)
        .build();

    if let QueryDescriptor::Scan { table_name, filters, limit, .. } = desc {
        assert_eq!(table_name, "comment");
        assert_eq!(filters[0].value, Value::U64(42));
        assert_eq!(limit, Some(10));
    } else {
        panic!("expected Scan");
    }
}

#[test]
fn test_serialize_and_query() {
    let user = User {
        id: 1,
        username: "alice".to_string(),
        active: true,
    };

    let bytes = user.to_store_bytes();
    let decoded = User::from_store_bytes(&bytes).unwrap();
    assert_eq!(user, decoded);

    let schema = User::schema();
    assert_eq!(schema.columns[0].name, "id");
    assert!(schema.columns[0].is_primary_key);
    assert_eq!(schema.columns[1].column_type, ColumnType::String);
    assert_eq!(schema.columns[2].column_type, ColumnType::Bool);
}

#[test]
fn test_transaction_descriptor() {
    let user = User { id: 1, username: "bob".to_string(), active: true };

    let desc = QueryDescriptor::Transaction {
        ops: vec![
            QueryDescriptor::Insert {
                table_name: User::TABLE_NAME,
                key_bytes: user.primary_key_bytes(),
                row_bytes: user.to_store_bytes(),
            },
            QueryDescriptor::Delete {
                table_name: "comment",
                key_bytes: 42u64.to_le_bytes().to_vec(),
            },
        ],
    };

    if let QueryDescriptor::Transaction { ops } = desc {
        assert_eq!(ops.len(), 2);
        assert!(matches!(ops[0], QueryDescriptor::Insert { .. }));
        assert!(matches!(ops[1], QueryDescriptor::Delete { .. }));
    }
}

#[test]
fn test_multiple_structs_same_crate() {
    // Verify two derived structs coexist without conflicts
    assert_eq!(User::TABLE_NAME, "user");
    assert_eq!(Comment::TABLE_NAME, "comment");
    assert_eq!(User::schema().columns.len(), 3);
    assert_eq!(Comment::schema().columns.len(), 4);
    assert!(Comment::schema().columns[1].is_indexed); // post_id
}

#[test]
fn test_filter_chaining() {
    let desc = Query::<Comment>::new(Comment::TABLE_NAME)
        .filter(Comment::POST_ID.eq(1u64))
        .filter(Comment::AUTHOR_ID.gt(10u64))
        .order_by(Comment::ID, Direction::Desc)
        .limit(5)
        .offset(10)
        .build();

    if let QueryDescriptor::Scan { filters, order_by, limit, offset, .. } = desc {
        assert_eq!(filters.len(), 2);
        assert_eq!(filters[0].op, FilterOp::Eq);
        assert_eq!(filters[1].op, FilterOp::Gt);
        assert_eq!(order_by, Some((0, Direction::Desc)));
        assert_eq!(limit, Some(5));
        assert_eq!(offset, Some(10));
    } else {
        panic!("expected Scan");
    }
}
