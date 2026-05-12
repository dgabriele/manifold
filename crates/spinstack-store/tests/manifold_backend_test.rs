use spinstack_store::schema::StoreRecord;
use spinstack_store::*;

#[derive(Store, Debug, PartialEq)]
struct User {
    #[primary_key]
    id: u64,
    name: String,
    active: bool,
}

#[derive(Store, Debug, PartialEq)]
struct Post {
    #[primary_key]
    id: u64,
    #[indexed]
    author_id: u64,
    content: String,
    created_at: i64,
}

fn create_store() -> (ManifoldStore, tempfile::TempDir) {
    let dir = tempfile::TempDir::new().unwrap();
    let db = manifold::column_family::ColumnFamilyDatabase::builder()
        .open(dir.path().join("test.db"))
        .unwrap();
    db.create_column_family("default", None).unwrap();
    let cf = db.column_family("default").unwrap();
    let store = ManifoldStore::new(cf);
    (store, dir)
}

#[test]
fn test_insert_and_get() {
    let (store, _dir) = create_store();
    store.ensure_schema::<User>().unwrap();

    let user = User {
        id: 1,
        name: "alice".into(),
        active: true,
    };
    store.insert(&user).unwrap();

    let fetched = store.get::<User>(1u64).unwrap();
    assert_eq!(fetched, Some(user));
}

#[test]
fn test_get_not_found() {
    let (store, _dir) = create_store();
    store.ensure_schema::<User>().unwrap();

    let fetched = store.get::<User>(999u64).unwrap();
    assert_eq!(fetched, None);
}

#[test]
fn test_update() {
    let (store, _dir) = create_store();
    store.ensure_schema::<User>().unwrap();

    let mut user = User {
        id: 1,
        name: "alice".into(),
        active: true,
    };
    store.insert(&user).unwrap();

    user.name = "alice_updated".into();
    user.active = false;
    store.update(&user).unwrap();

    let fetched = store.get::<User>(1u64).unwrap().unwrap();
    assert_eq!(fetched.name, "alice_updated");
    assert!(!fetched.active);
}

#[test]
fn test_delete() {
    let (store, _dir) = create_store();
    store.ensure_schema::<User>().unwrap();

    let user = User {
        id: 1,
        name: "alice".into(),
        active: true,
    };
    store.insert(&user).unwrap();
    store.delete::<User>(1u64).unwrap();

    assert_eq!(store.get::<User>(1u64).unwrap(), None);
}

#[test]
fn test_duplicate_insert() {
    let (store, _dir) = create_store();
    store.ensure_schema::<User>().unwrap();

    let user = User {
        id: 1,
        name: "alice".into(),
        active: true,
    };
    store.insert(&user).unwrap();

    let result = store.insert(&user);
    assert!(matches!(result, Err(StoreError::DuplicateKey)));
}

#[test]
fn test_fetch_with_filter() {
    let (store, _dir) = create_store();
    store.ensure_schema::<Post>().unwrap();

    for i in 1..=10u64 {
        let post = Post {
            id: i,
            author_id: i % 3,
            content: format!("post {i}"),
            created_at: i as i64 * 1000,
        };
        store.insert(&post).unwrap();
    }

    // Filter: author_id == 1
    let results = store
        .fetch(Query::<Post>::new(Post::TABLE_NAME).filter(Post::AUTHOR_ID.eq(1u64)))
        .unwrap();

    assert_eq!(results.len(), 4); // ids 1, 4, 7, 10
    for post in &results {
        assert_eq!(post.author_id, 1);
    }
}

#[test]
fn test_fetch_with_limit() {
    let (store, _dir) = create_store();
    store.ensure_schema::<Post>().unwrap();

    for i in 1..=20u64 {
        let post = Post {
            id: i,
            author_id: 1,
            content: format!("post {i}"),
            created_at: i as i64,
        };
        store.insert(&post).unwrap();
    }

    let results = store
        .fetch(Query::<Post>::new(Post::TABLE_NAME).limit(5))
        .unwrap();

    assert_eq!(results.len(), 5);
}

#[test]
fn test_count() {
    let (store, _dir) = create_store();
    store.ensure_schema::<Post>().unwrap();

    for i in 1..=10u64 {
        let post = Post {
            id: i,
            author_id: i % 3,
            content: format!("post {i}"),
            created_at: 0,
        };
        store.insert(&post).unwrap();
    }

    // Total count (O(1))
    let total = store
        .count(Query::<Post>::new(Post::TABLE_NAME))
        .unwrap();
    assert_eq!(total, 10);

    // Filtered count
    let filtered = store
        .count(Query::<Post>::new(Post::TABLE_NAME).filter(Post::AUTHOR_ID.eq(0u64)))
        .unwrap();
    assert_eq!(filtered, 3); // ids 3, 6, 9
}

#[test]
fn test_transaction() {
    let (store, _dir) = create_store();
    store.ensure_schema::<User>().unwrap();
    store.ensure_schema::<Post>().unwrap();

    store
        .transaction(|txn| {
            txn.insert(&User {
                id: 1,
                name: "alice".into(),
                active: true,
            })?;
            txn.insert(&Post {
                id: 1,
                author_id: 1,
                content: "hello".into(),
                created_at: 1000,
            })?;
            Ok(())
        })
        .unwrap();

    // Both should be visible
    assert!(store.get::<User>(1u64).unwrap().is_some());
    assert!(store.get::<Post>(1u64).unwrap().is_some());
}

#[test]
fn test_transaction_read_your_writes() {
    let (store, _dir) = create_store();
    store.ensure_schema::<User>().unwrap();

    store
        .transaction(|txn| {
            txn.insert(&User {
                id: 1,
                name: "alice".into(),
                active: true,
            })?;
            let user = txn.get::<User>(1u64)?;
            assert!(user.is_some());
            assert_eq!(user.unwrap().name, "alice");
            Ok(())
        })
        .unwrap();
}
