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
// Idempotency-key store
// ---------------------------------------------------------------------------

/// Outcome recorded for a client-supplied `Idempotency-Key`.
///
/// `InFlight` is written *before* the transaction-submitting work begins, so
/// a concurrent duplicate request (the client retried before the first
/// response came back) sees the reservation rather than racing the same
/// submission a second time. `Completed` replays the original response body
/// once the first request finishes successfully.
#[derive(Debug, Clone)]
pub enum IdempotencyEntry {
    InFlight,
    Completed(String),
}

/// In-memory record of recently-seen idempotency keys, keyed by the raw
/// `Idempotency-Key` header value.
///
/// Mirrors `DbStore`: a plain `HashMap` guarded by an `RwLock` kept separate
/// from `DbStore` so idempotency bookkeeping never contends with the
/// business-data lock.
#[derive(Debug, Default)]
pub struct IdempotencyStore {
    pub entries: HashMap<String, IdempotencyEntry>,
}

/// Shared, thread-safe handle to the idempotency store.
pub type SharedIdempotencyStore = Arc<RwLock<IdempotencyStore>>;

/// Create a new, empty shared idempotency-store handle.
pub fn new_shared_idempotency_store() -> SharedIdempotencyStore {
    Arc::new(RwLock::new(IdempotencyStore::default()))
}

/// Acquire the idempotency-store write lock, recovering gracefully from lock
/// poison (see the module-level note on lock-poison recovery, #473).
pub fn acquire_idempotency(
    store: &SharedIdempotencyStore,
) -> std::sync::RwLockWriteGuard<'_, IdempotencyStore> {
    store.write().unwrap_or_else(|e| e.into_inner())
}

/// Acquire the idempotency-store read lock, recovering gracefully from lock
/// poison.
pub fn read_idempotency(
    store: &SharedIdempotencyStore,
) -> std::sync::RwLockReadGuard<'_, IdempotencyStore> {
    store.read().unwrap_or_else(|e| e.into_inner())
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

    #[test]
    fn test_idempotency_store_starts_empty() {
        let store = new_shared_idempotency_store();
        assert!(read_idempotency(&store).entries.is_empty());
    }

    #[test]
    fn test_idempotency_store_records_in_flight_then_completed() {
        let store = new_shared_idempotency_store();

        acquire_idempotency(&store)
            .entries
            .insert("key-1".to_string(), IdempotencyEntry::InFlight);
        assert!(matches!(
            read_idempotency(&store).entries.get("key-1"),
            Some(IdempotencyEntry::InFlight)
        ));

        acquire_idempotency(&store).entries.insert(
            "key-1".to_string(),
            IdempotencyEntry::Completed(r#"{"ok":true}"#.to_string()),
        );
        assert!(matches!(
            read_idempotency(&store).entries.get("key-1"),
            Some(IdempotencyEntry::Completed(body)) if body == r#"{"ok":true}"#
        ));
    }

    #[test]
    fn test_idempotency_store_poison_recovery() {
        let store = new_shared_idempotency_store();

        let store_clone = Arc::clone(&store);
        let _ = std::panic::catch_unwind(move || {
            let _guard = store_clone.write().unwrap();
            panic!("simulated panic");
        });

        let guard = acquire_idempotency(&store);
        assert!(guard.entries.is_empty(), "recovered store should be intact");
    }
}
