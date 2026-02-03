//! Safe Lock Utilities
//!
//! This module provides utilities for safely acquiring locks on Mutex and RwLock
//! without panicking on poisoned locks. Instead of using `.lock().unwrap()` which
//! will panic if a previous thread panicked while holding the lock, these utilities
//! return proper errors that can be handled gracefully.
//!
//! ## Why Poisoned Locks Matter
//!
//! When a thread panics while holding a lock, the lock becomes "poisoned" because
//! the data it protects may be in an inconsistent state. The default `.unwrap()`
//! behavior panics to prevent accessing potentially corrupted data.
//!
//! However, in many cases the data is still valid (e.g., the panic was in unrelated
//! code that happened to hold the lock), and we'd rather log the error and continue
//! operating than crash the entire application.
//!
//! ## Usage
//!
//! ```rust

#![allow(dead_code)]
//! use crate::safe_lock::{safe_lock, safe_read, safe_write};
//!
//! // For Mutex
//! let guard = safe_lock(&my_mutex, "my_data")?;
//!
//! // For RwLock read
//! let guard = safe_read(&my_rwlock, "my_data")?;
//!
//! // For RwLock write
//! let guard = safe_write(&my_rwlock, "my_data")?;
//! ```

use std::sync::{Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use tracing::error;

use crate::error::AppError;

/// Safely acquire a Mutex lock, returning an error instead of panicking on poison.
///
/// # Arguments
/// * `mutex` - The Mutex to lock
/// * `name` - A descriptive name for the lock (used in error messages)
///
/// # Returns
/// * `Ok(MutexGuard)` if the lock was acquired successfully
/// * `Err(AppError::LockError)` if the lock was poisoned
pub fn safe_lock<'a, T>(mutex: &'a Mutex<T>, name: &str) -> Result<MutexGuard<'a, T>, AppError> {
    mutex.lock().map_err(|e: PoisonError<MutexGuard<'_, T>>| {
        error!(
            "Mutex '{}' was poisoned by a panicked thread. The application may be in an inconsistent state.",
            name
        );
        AppError::LockError(format!(
            "Mutex '{}' poisoned: {}. A previous thread panicked while holding this lock.",
            name, e
        ))
    })
}

/// Safely acquire a RwLock read lock, returning an error instead of panicking on poison.
///
/// # Arguments
/// * `rwlock` - The RwLock to read-lock
/// * `name` - A descriptive name for the lock (used in error messages)
///
/// # Returns
/// * `Ok(RwLockReadGuard)` if the lock was acquired successfully
/// * `Err(AppError::LockError)` if the lock was poisoned
pub fn safe_read<'a, T>(
    rwlock: &'a RwLock<T>,
    name: &str,
) -> Result<RwLockReadGuard<'a, T>, AppError> {
    rwlock
        .read()
        .map_err(|e: PoisonError<RwLockReadGuard<'_, T>>| {
            error!(
            "RwLock '{}' was poisoned by a panicked thread (read). The application may be in an inconsistent state.",
            name
        );
            AppError::LockError(format!(
                "RwLock '{}' poisoned (read): {}. A previous thread panicked while holding this lock.",
                name, e
            ))
        })
}

/// Safely acquire a RwLock write lock, returning an error instead of panicking on poison.
///
/// # Arguments
/// * `rwlock` - The RwLock to write-lock
/// * `name` - A descriptive name for the lock (used in error messages)
///
/// # Returns
/// * `Ok(RwLockWriteGuard)` if the lock was acquired successfully
/// * `Err(AppError::LockError)` if the lock was poisoned
pub fn safe_write<'a, T>(
    rwlock: &'a RwLock<T>,
    name: &str,
) -> Result<RwLockWriteGuard<'a, T>, AppError> {
    rwlock
        .write()
        .map_err(|e: PoisonError<RwLockWriteGuard<'_, T>>| {
            error!(
            "RwLock '{}' was poisoned by a panicked thread (write). The application may be in an inconsistent state.",
            name
        );
            AppError::LockError(format!(
            "RwLock '{}' poisoned (write): {}. A previous thread panicked while holding this lock.",
            name, e
        ))
        })
}

/// Safely acquire a Mutex lock, recovering the guard even if poisoned.
///
/// This is useful when you know the data is still valid despite the poison,
/// and you want to continue operating. The error is logged but the guard
/// is returned anyway.
///
/// # Arguments
/// * `mutex` - The Mutex to lock
/// * `name` - A descriptive name for the lock (used in logging)
///
/// # Returns
/// The MutexGuard (recovered from poison if necessary)
pub fn safe_lock_or_recover<'a, T>(mutex: &'a Mutex<T>, name: &str) -> MutexGuard<'a, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poison_error) => {
            error!(
                "Mutex '{}' was poisoned - recovering guard. Data may be inconsistent.",
                name
            );
            poison_error.into_inner()
        }
    }
}

/// Safely acquire a RwLock read lock, recovering even if poisoned.
///
/// # Arguments
/// * `rwlock` - The RwLock to read-lock
/// * `name` - A descriptive name for the lock (used in logging)
///
/// # Returns
/// The RwLockReadGuard (recovered from poison if necessary)
pub fn safe_read_or_recover<'a, T>(rwlock: &'a RwLock<T>, name: &str) -> RwLockReadGuard<'a, T> {
    match rwlock.read() {
        Ok(guard) => guard,
        Err(poison_error) => {
            error!(
                "RwLock '{}' was poisoned (read) - recovering guard. Data may be inconsistent.",
                name
            );
            poison_error.into_inner()
        }
    }
}

/// Safely acquire a RwLock write lock, recovering even if poisoned.
///
/// # Arguments
/// * `rwlock` - The RwLock to write-lock
/// * `name` - A descriptive name for the lock (used in logging)
///
/// # Returns
/// The RwLockWriteGuard (recovered from poison if necessary)
pub fn safe_write_or_recover<'a, T>(rwlock: &'a RwLock<T>, name: &str) -> RwLockWriteGuard<'a, T> {
    match rwlock.write() {
        Ok(guard) => guard,
        Err(poison_error) => {
            error!(
                "RwLock '{}' was poisoned (write) - recovering guard. Data may be inconsistent.",
                name
            );
            poison_error.into_inner()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_safe_lock_success() {
        let mutex = Mutex::new(42);
        let guard = safe_lock(&mutex, "test_mutex").unwrap();
        assert_eq!(*guard, 42);
    }

    #[test]
    fn test_safe_read_success() {
        let rwlock = RwLock::new(42);
        let guard = safe_read(&rwlock, "test_rwlock").unwrap();
        assert_eq!(*guard, 42);
    }

    #[test]
    fn test_safe_write_success() {
        let rwlock = RwLock::new(42);
        let mut guard = safe_write(&rwlock, "test_rwlock").unwrap();
        *guard = 100;
        assert_eq!(*guard, 100);
    }

    #[test]
    fn test_safe_lock_poisoned() {
        let mutex = Arc::new(Mutex::new(42));
        let mutex_clone = mutex.clone();

        // Spawn a thread that will panic while holding the lock
        let handle = thread::spawn(move || {
            let _guard = mutex_clone.lock().unwrap();
            panic!("intentional panic to poison the lock");
        });

        // Wait for the thread to panic
        let _ = handle.join();

        // Now try to acquire the poisoned lock
        let result = safe_lock(&mutex, "poisoned_mutex");
        assert!(result.is_err());
        if let Err(AppError::LockError(msg)) = result {
            assert!(msg.contains("poisoned"));
        } else {
            panic!("Expected LockError");
        }
    }

    #[test]
    fn test_safe_lock_or_recover_poisoned() {
        let mutex = Arc::new(Mutex::new(42));
        let mutex_clone = mutex.clone();

        // Spawn a thread that will panic while holding the lock
        let handle = thread::spawn(move || {
            let _guard = mutex_clone.lock().unwrap();
            panic!("intentional panic to poison the lock");
        });

        // Wait for the thread to panic
        let _ = handle.join();

        // Now try to recover the poisoned lock
        let guard = safe_lock_or_recover(&mutex, "poisoned_mutex");
        assert_eq!(*guard, 42); // Data should still be valid
    }
}
