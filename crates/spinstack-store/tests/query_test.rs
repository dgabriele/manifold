use spinstack_store::*;
use spinstack_store::descriptor::QueryDescriptor;

struct Post;
impl Post {
    const ID: Field<u64> = Field::new(0);
    const AUTHOR_ID: Field<u64> = Field::new(1);
    const CREATED_AT: Field<i64> = Field::new(3);
}

#[test]
fn test_scan_with_filter_and_limit() {
    let desc = Query::<Post>::new("posts")
        .filter(Post::AUTHOR_ID.eq(42u64))
        .order_by(Post::CREATED_AT, Direction::Desc)
        .limit(20)
        .build();
    match desc {
        QueryDescriptor::Scan { table_name, filters, order_by, limit, .. } => {
            assert_eq!(table_name, "posts");
            assert_eq!(filters.len(), 1);
            assert_eq!(filters[0].column_index, 1);
            assert_eq!(filters[0].op, FilterOp::Eq);
            assert_eq!(filters[0].value, Value::U64(42));
            assert_eq!(order_by, Some((3, Direction::Desc)));
            assert_eq!(limit, Some(20));
        }
        _ => panic!("expected Scan descriptor"),
    }
}

#[test]
fn test_aggregate_count() {
    let desc = Query::<Post>::new("posts")
        .filter(Post::AUTHOR_ID.eq(5u64))
        .build_count();
    match desc {
        QueryDescriptor::Aggregate { table_name, filters, func, column } => {
            assert_eq!(table_name, "posts");
            assert_eq!(filters.len(), 1);
            assert_eq!(func, AggregateFunc::Count);
            assert!(column.is_none());
        }
        _ => panic!("expected Aggregate descriptor"),
    }
}
