use manifold_sql::Database;
use tempfile::TempDir;

pub fn test_db() -> (Database, TempDir) {
    let dir = TempDir::new().unwrap();
    let db = Database::open(dir.path().join("test.db")).unwrap();
    (db, dir)
}
