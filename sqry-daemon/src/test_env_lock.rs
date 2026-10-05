//! The crate-wide test lock on the process environment, as a mutex that
//! says who holds it (test builds only).
//!
//! [`crate::TEST_ENV_LOCK`] is a `std::sync::Mutex<()>` wrapped in
//! [`TestEnvLock`]. Taking it records a token of the thread that took it, and
//! dropping the [`TestEnvGuard`] clears that record before the mutex is
//! released, so the record is set only while the mutex is held: it may read
//! "free" for the few instructions after the mutex is taken and before it is
//! released, never "held" while the mutex is free, and never names a thread
//! still waiting for the mutex. The runtime environment trace (`env_trace`,
//! Linux with glibc) reads [`TestEnvLock::holder`] at every environment
//! access a lib test makes, which is how it knows whether the lock is held
//! there, without looking at the stack.
//!
//! `lock()` keeps `Mutex::lock`'s signature with the guard type replaced, so
//! every call site reads as it did: `TEST_ENV_LOCK.lock()`, then `unwrap()`,
//! `expect(..)` or `unwrap_or_else(|e| e.into_inner())`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LockResult, Mutex, MutexGuard, PoisonError};

/// A mutex over nothing that records the thread holding it.
#[derive(Debug)]
pub(crate) struct TestEnvLock {
    mutex: Mutex<()>,
    /// The token of the thread holding the mutex (`thread_token`), or 0.
    holder: AtomicU64,
}

/// The guard [`TestEnvLock::lock`] returns: the mutex's own guard, and the
/// record of its holder, cleared when the guard drops.
#[derive(Debug)]
pub(crate) struct TestEnvGuard {
    holder: &'static AtomicU64,
    _guard: MutexGuard<'static, ()>,
}

impl TestEnvLock {
    /// A free lock.
    pub(crate) const fn new() -> Self {
        Self {
            mutex: Mutex::new(()),
            holder: AtomicU64::new(0),
        }
    }

    /// Takes the lock as `Mutex::lock` does, poisoning included, and records
    /// the calling thread as its holder.
    pub(crate) fn lock(&'static self) -> LockResult<TestEnvGuard> {
        match self.mutex.lock() {
            Ok(guard) => Ok(self.held(guard)),
            Err(poisoned) => Err(PoisonError::new(self.held(poisoned.into_inner()))),
        }
    }

    fn held(&'static self, guard: MutexGuard<'static, ()>) -> TestEnvGuard {
        self.holder.store(thread_token(), Ordering::SeqCst);
        TestEnvGuard {
            holder: &self.holder,
            _guard: guard,
        }
    }

    /// The token of the thread that holds the lock, or 0 when it is free.
    /// On Linux the token is the thread's kernel id (`gettid`).
    pub(crate) fn holder(&self) -> u64 {
        self.holder.load(Ordering::SeqCst)
    }
}

impl Drop for TestEnvGuard {
    fn drop(&mut self) {
        // Cleared before `_guard` drops, so the record never outlives the
        // mutex being held.
        self.holder.store(0, Ordering::SeqCst);
    }
}

/// A non-zero token naming the calling thread: its kernel thread id on
/// Linux, where the environment trace compares it with the id of the thread
/// making an access; 1 elsewhere, where only "held or not" is read.
fn thread_token() -> u64 {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `gettid` has no preconditions and cannot fail.
        let tid = unsafe { libc::gettid() };
        u64::try_from(tid).unwrap_or(1).max(1)
    }
    #[cfg(not(target_os = "linux"))]
    {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::TestEnvLock;

    static LOCK: TestEnvLock = TestEnvLock::new();

    /// The record is set exactly while the guard lives, on the poisoned path
    /// too, and names the thread that took the lock.
    #[test]
    fn the_holder_is_recorded_while_the_guard_lives() {
        assert_eq!(LOCK.holder(), 0, "a fresh lock has no holder");
        let guard = LOCK.lock().expect("an unpoisoned lock");
        let holder = LOCK.holder();
        assert_ne!(holder, 0, "the holder is recorded while the guard lives");
        #[cfg(target_os = "linux")]
        {
            // SAFETY: `gettid` has no preconditions and cannot fail.
            let tid = u64::try_from(unsafe { libc::gettid() }).expect("a positive tid");
            assert_eq!(holder, tid, "the holder is the thread that took the lock");
        }
        drop(guard);
        assert_eq!(LOCK.holder(), 0, "dropping the guard clears the holder");

        let poisoner = std::thread::spawn(|| {
            let _guard = LOCK.lock();
            panic!("poison the lock on purpose");
        });
        assert!(poisoner.join().is_err(), "the poisoning thread panicked");
        assert_eq!(
            LOCK.holder(),
            0,
            "a guard dropped while unwinding clears the holder"
        );
        let poisoned = LOCK.lock().expect_err("the lock is poisoned");
        let guard = poisoned.into_inner();
        assert_ne!(LOCK.holder(), 0, "a poisoned lock records its holder too");
        drop(guard);
        assert_eq!(LOCK.holder(), 0, "and clears it on drop");
    }

    /// The record is set only after the mutex is taken: while one thread
    /// holds the lock and another waits in `lock()`, the record names the
    /// first, and the second only once it holds the mutex.
    #[test]
    fn a_thread_waiting_for_the_mutex_is_never_the_holder() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::{Duration, Instant};
        static WAITED: TestEnvLock = TestEnvLock::new();
        static STARTED: AtomicBool = AtomicBool::new(false);
        let guard = WAITED.lock().expect("an unpoisoned lock");
        let holder = WAITED.holder();
        assert_ne!(holder, 0, "the holder is recorded");
        let waiter = std::thread::spawn(|| {
            STARTED.store(true, Ordering::SeqCst);
            let _guard = WAITED.lock().expect("an unpoisoned lock");
            WAITED.holder()
        });
        while !STARTED.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }
        // The waiter is in `lock()` or about to be; for a while after, the
        // record still names the thread that holds the mutex.
        let until = Instant::now() + Duration::from_millis(300);
        while Instant::now() < until {
            assert_eq!(
                WAITED.holder(),
                holder,
                "a thread waiting for the mutex is never recorded as its holder"
            );
            std::thread::yield_now();
        }
        drop(guard);
        let recorded = waiter.join().expect("the waiting thread");
        assert_ne!(
            recorded, 0,
            "the waiter is recorded once it holds the mutex"
        );
        #[cfg(target_os = "linux")]
        assert_ne!(recorded, holder, "and the record names the waiter then");
    }
}
