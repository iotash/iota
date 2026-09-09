//! Lock helpers shared across the crate.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Locks `m`, riding over poisoning: the state behind these locks is display or bookkeeping state that every
/// access re-establishes, so a panicked holder (a test thread, typically) must not take the loop down with it.
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
