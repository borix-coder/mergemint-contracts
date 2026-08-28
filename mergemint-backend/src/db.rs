// mergemint-backend/src/db.rs
//
// Database connection pool and shared state helpers.
//
// ## Lock-poison recovery (#473)
//
// Rust's lock APIs return `Err(PoisonError)` if a thread panicked while
// holding the lock. Calling `.unwrap()` would re-panic every subsequent
// caller, effectively taking the whole service down for what is often a
// transient edge case.
//
// We use `.unwrap_or_else(|e| e.into_inner())` instead: when the lock is
// poisoned we recover the inner value and continue under the assumption that
// the data is still in a consistent-enough state to serve requests.  If the
// data truly is corrupt the next business-logic validation will catch it and
// return an error to the client rather than crashing the process.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Lightweight in-memory store used during development / integration tests.
/// Production deployments replace this with a real database pool.
#[derive(Debug, Default)]
pub struct DbStore {
    pub records: HashMap<String, String>,
}

/// Shared, thread-safe handle to the database store.
///
/// The store uses an `RwLock` so API read paths can proceed concurrently while
/// writes still take exclusive access. This mirrors the production goal of
/// avoiding a single mutex-guarded connection that serializes every DB access.
pub type SharedDb = Arc<RwLock<DbStore>>;

/// Create a new, empty shared database handle.
pub fn new_shared_db() -> SharedDb {
    Arc::new(RwLock::new(DbStore::default()))
}

/// Acquire the database write lock, recovering gracefully from lock poison.
///
/// If a previous thread panicked while holding this lock, `.into_inner()`
/// extracts the guarded value so the service can keep running instead of
/// propagating the panic to every subsequent request.
#[allow(dead_code)]
pub fn acquire_db(db: &SharedDb) -> std::sync::RwLockWriteGuard<'_, DbStore> {
    db.write().unwrap_or_else(|e| e.into_inner())
}

/// Acquire the database read lock, recovering gracefully from lock poison.
pub fn read_db(db: &SharedDb) -> std::sync::RwLockReadGuard<'_, DbStore> {
    db.read().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------
// Connection pool
// ---------------------------------------------------------------------------
//
// ## Bounded-wait pool exhaustion
//
// The store above is guarded by a single `RwLock`, so it has no notion of a
// fixed number of "connections" the way a real `sqlx::PgPool` does. `DbPool`
// adds that capacity bound ahead of the real Postgres pool swap-in: callers
// check out a `PooledConnection` from a fixed-size semaphore, and once the
// pool is saturated, `acquire` waits at most `POOL_ACQUIRE_TIMEOUT` before
// returning `PoolExhausted` — a clear, fast error — rather than blocking the
// caller indefinitely.

/// Default number of concurrent connections a [`DbPool`] hands out before
/// callers must wait for one to be released, mirroring a real DB pool's
/// `max_connections` setting.
#[allow(dead_code)]
pub const DEFAULT_POOL_SIZE: usize = 10;

/// Upper bound on how long [`DbPool::acquire`] waits for a free connection.
/// Bounding the wait is what turns pool exhaustion into a fast, explicit
/// error instead of a request that hangs indefinitely.
#[allow(dead_code)]
pub const POOL_ACQUIRE_TIMEOUT: Duration = Duration::from_millis(500);

/// Returned by [`DbPool::acquire`] when no connection became available
/// within `POOL_ACQUIRE_TIMEOUT`.
#[derive(Debug)]
#[allow(dead_code)]
pub struct PoolExhausted;

impl std::fmt::Display for PoolExhausted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "connection pool exhausted: no connection became available within the timeout"
        )
    }
}

impl std::error::Error for PoolExhausted {}

/// A capacity-bounded pool over [`SharedDb`].
#[derive(Clone)]
#[allow(dead_code)]
pub struct DbPool {
    db: SharedDb,
    permits: Arc<Semaphore>,
}

#[allow(dead_code)]
impl DbPool {
    /// Build a pool with the default capacity.
    pub fn new(db: SharedDb) -> Self {
        Self::with_capacity(db, DEFAULT_POOL_SIZE)
    }

    /// Build a pool with an explicit capacity — used by tests to saturate a
    /// small pool without checking out hundreds of connections.
    pub fn with_capacity(db: SharedDb, capacity: usize) -> Self {
        Self {
            db,
            permits: Arc::new(Semaphore::new(capacity)),
        }
    }

    /// Check out a connection, waiting up to `POOL_ACQUIRE_TIMEOUT` for one
    /// to free up. Returns `PoolExhausted` rather than hanging if the pool
    /// stays saturated for the whole timeout window.
    pub async fn acquire(&self) -> Result<PooledConnection, PoolExhausted> {
        let permit =
            tokio::time::timeout(POOL_ACQUIRE_TIMEOUT, self.permits.clone().acquire_owned())
                .await
                .map_err(|_elapsed| PoolExhausted)?
                .expect("pool semaphore is never closed");

        Ok(PooledConnection {
            db: self.db.clone(),
            _permit: permit,
        })
    }
}

/// A checked-out pool connection. Dropping it releases the permit back to
/// the pool.
#[allow(dead_code)]
pub struct PooledConnection {
    db: SharedDb,
    _permit: OwnedSemaphorePermit,
}

#[allow(dead_code)]
impl PooledConnection {
    pub fn read(&self) -> std::sync::RwLockReadGuard<'_, DbStore> {
        read_db(&self.db)
    }

    pub fn write(&self) -> std::sync::RwLockWriteGuard<'_, DbStore> {
        acquire_db(&self.db)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Placeholder test — verifies that the test harness compiles and is wired
    /// correctly.  See issue #487.
    #[test]
    fn it_compiles() {}

    #[test]
    fn test_acquire_db_normal() {
        let db = new_shared_db();
        let mut guard = acquire_db(&db);
        guard.records.insert("key".to_string(), "value".to_string());
        assert_eq!(guard.records.get("key").map(|s| s.as_str()), Some("value"));
    }

    #[test]
    fn test_acquire_db_poison_recovery() {
        let db = new_shared_db();

        // Simulate a panic while holding the lock.
        let db_clone = Arc::clone(&db);
        let _ = std::panic::catch_unwind(move || {
            let _guard = db_clone.write().unwrap();
            panic!("simulated panic");
        });

        // The lock is now poisoned; acquire_db must not propagate the poison.
        let guard = acquire_db(&db);
        assert!(guard.records.is_empty(), "recovered store should be intact");
    }

    #[test]
    fn test_concurrent_read_guards_are_allowed() {
        let db = new_shared_db();
        let read_a = read_db(&db);
        let read_b = read_db(&db);

        assert!(read_a.records.is_empty());
        assert!(read_b.records.is_empty());
    }

    // ── Connection pool exhaustion ─────────────────────────────────────────

    /// Saturating the pool must yield a bounded-time error on the next
    /// acquire, never an indefinite hang.
    #[tokio::test]
    async fn test_pool_exhaustion_returns_bounded_time_error_not_hang() {
        let db = new_shared_db();
        let pool = DbPool::with_capacity(db, 2);

        let _held_1 = pool
            .acquire()
            .await
            .expect("first connection should succeed");
        let _held_2 = pool
            .acquire()
            .await
            .expect("second connection should succeed");

        let start = std::time::Instant::now();
        let result = pool.acquire().await;
        let elapsed = start.elapsed();

        assert!(
            result.is_err(),
            "acquiring beyond pool capacity must return an error, not succeed"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "pool exhaustion must fail within a bounded time instead of hanging; took {elapsed:?}"
        );
    }

    /// Once a held connection is dropped, its permit must be returned to the
    /// pool so the next acquire succeeds.
    #[tokio::test]
    async fn test_pool_connection_is_released_back_after_drop() {
        let db = new_shared_db();
        let pool = DbPool::with_capacity(db, 1);

        {
            let _held = pool
                .acquire()
                .await
                .expect("first connection should succeed");
            assert!(
                pool.acquire().await.is_err(),
                "pool of capacity 1 must be exhausted while the only connection is held"
            );
        }

        assert!(
            pool.acquire().await.is_ok(),
            "connection should be available again once the held one is dropped"
        );
    }
}
