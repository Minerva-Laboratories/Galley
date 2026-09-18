//! One SQLite database per server, in WAL mode, with a small r2d2 pool. The migrations are
//! numbered SQL files. The server applies them on startup inside a `schema_version` check. Nobody
//! edits a migration after merge.

use std::path::Path;
use std::time::Duration;

use r2d2::PooledConnection;
use r2d2_sqlite::SqliteConnectionManager;

/// Numbered migrations, embedded so a released binary carries its own schema.
const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("../migrations/0001_init.sql")),
    (2, include_str!("../migrations/0002_governance.sql")),
    (3, include_str!("../migrations/0003_checkpoints.sql")),
    (4, include_str!("../migrations/0004_agents.sql")),
];

pub type Conn = PooledConnection<SqliteConnectionManager>;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("connection pool error: {0}")]
    Pool(#[from] r2d2::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Clone)]
pub struct Db {
    pool: r2d2::Pool<SqliteConnectionManager>,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db").finish()
    }
}

impl Db {
    /// Open (creating if needed) the database at `path` and run migrations.
    pub fn open(path: &Path) -> Result<Db, DbError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Enable WAL once, at the start. The journal_mode setting is persistent in the file
        // header. A per-connection setting races and reports "database is locked" when the pool
        // warms up concurrently.
        {
            let c = rusqlite::Connection::open(path)?;
            c.busy_timeout(Duration::from_secs(10))?;
            c.pragma_update(None, "journal_mode", "WAL")?;
        }
        let manager = SqliteConnectionManager::file(path).with_init(|c| {
            c.busy_timeout(Duration::from_secs(10))?;
            c.pragma_update(None, "synchronous", "NORMAL")?;
            c.pragma_update(None, "foreign_keys", "ON")?;
            Ok(())
        });
        let pool = r2d2::Pool::builder().max_size(8).build(manager)?;
        let db = Db { pool };
        db.migrate()?;
        Ok(db)
    }

    /// An in-memory database for the tests. Each call gets a shared-cache database with a unique
    /// name. The pooled connections then share data with each other, but not with a parallel test.
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Db, DbError> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let name = format!("file:galleytest-{}?mode=memory&cache=shared", SEQ.fetch_add(1, Ordering::Relaxed));
        let manager = SqliteConnectionManager::file(name).with_init(|c| {
            c.busy_timeout(Duration::from_secs(5))?;
            c.pragma_update(None, "foreign_keys", "ON")?;
            Ok(())
        });
        let pool = r2d2::Pool::builder().max_size(4).build(manager)?;
        let db = Db { pool };
        db.migrate()?;
        Ok(db)
    }

    pub fn conn(&self) -> Result<Conn, DbError> {
        Ok(self.pool.get()?)
    }

    fn migrate(&self) -> Result<(), DbError> {
        let conn = self.conn()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);",
        )?;
        let current: i64 = conn
            .query_row("SELECT COALESCE(MAX(version), 0) FROM schema_version", [], |r| r.get(0))?;
        for (version, sql) in MIGRATIONS {
            if *version <= current {
                continue;
            }
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(sql)?;
            tx.execute(
                "INSERT INTO schema_version (version, applied_at) VALUES (?1, ?2)",
                rusqlite::params![version, chrono::Utc::now().to_rfc3339()],
            )?;
            tx.commit()?;
            tracing::info!(version, "applied migration");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_apply_once_and_are_idempotent() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='users'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        // A second call to migrate changes nothing.
        db.migrate().unwrap();
        let versions: i64 = conn.query_row("SELECT COUNT(*) FROM schema_version", [], |r| r.get(0)).unwrap();
        assert_eq!(versions as usize, MIGRATIONS.len());
    }
}
