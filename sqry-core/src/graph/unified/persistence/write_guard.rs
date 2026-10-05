//! The persist lock, interrupted-persist recovery and the persist gate,
//! used by
//! [`persist_durable_graph_transaction`](crate::graph::unified::build::entrypoint::persist_durable_graph_transaction).
//! The race analysis and the windows these leave are recorded in decisions
//! D-i8-1 and D-i8-6 of the surface-parity progress record.
//!
//! - [`IndexWriteLock`]: an advisory exclusive lock (`flock` on Unix,
//!   `LockFileEx` on Windows, through [`std::fs::File::lock`]) on
//!   [`PERSIST_LOCK_FILE_NAME`] in `.sqry/graph/`. Each acquisition opens
//!   its own file description, so two threads exclude each other as two
//!   processes do. A thread that holds the lock for a directory takes it
//!   again without blocking; the re-entrant guard shares the hold, which is
//!   released when its last guard drops (decision D-i8-3). The guard is
//!   neither `Send` nor `Sync`.
//!
//!   Every hold records the identity of the file it locked (device and
//!   inode on Unix; none elsewhere, where only the file's existence is
//!   checked). `acquire` and `try_take` retry a lock won on a file the path
//!   no longer names, at most [`LOCK_IDENTITY_RETRIES`] times
//!   ([`UnstableLockFile`]); [`IndexWriteLock::is_current`] tells whether a
//!   hold is still on the file the path names.
//!
//!   [`IndexWriteLock::acquire_unless_cancelled`] waits with a blocking
//!   `flock` on a helper thread (one per lock file per process, keyed by
//!   the file's identity on Unix and by the path as spelled elsewhere) while the
//!   calling thread checks a cancel closure and the lock file's identity
//!   every [`LOCK_POLL_INTERVAL`]; it answers [`LockWait`].
//!   [`IndexWriteLock::acquire_existing_unless_cancelled`] and
//!   `try_acquire` do not create the directory and open the lock file
//!   through `open_lock_file_in_index`, which checks the index content
//!   (`holds_index_content`) once the file is open. The persist
//!   transaction records each commit on the lock file
//!   ([`IndexWriteLock::record_commit`]), and a hold reports whether it
//!   saw a committed index ([`holds_committed_index`],
//!   [`IndexWriteLock::saw_committed_index`]).
//! - [`recover_interrupted_persist`]: under a fresh hold, puts back the
//!   previous pair a persist set aside and did not commit, and discards the
//!   sets of committed ones. [`GraphStorage`](super::GraphStorage) runs it
//!   for a reader through `recover_for_reader`, and a fresh hold runs it as
//!   it is taken.
//! - The persist gate ([`close_persists_and_wait`]): closed by an exiting
//!   process; a persist that enters it after it closed is refused, and the
//!   close waits for every persist that entered before.

use std::cell::RefCell;
use std::fs::{self, File, OpenOptions};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use anyhow::{Context, Result, bail};

/// The persist lock file, in `.sqry/graph/`. No lock or persist path
/// removes it; removing the index (`sqry workspace clean`) does.
pub const PERSIST_LOCK_FILE_NAME: &str = ".persist.lock";

/// How often [`IndexWriteLock::acquire_unless_cancelled`] checks for a
/// cancellation and for a removed index while another holder has the lock.
pub const LOCK_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(20);

/// A persist refused because the index directory was removed after the
/// caller took the persist lock (decision D-i8-6): writing would recreate
/// the index the user removed. No index was written; a lock file, or the
/// directory, may have been created before the refusal. Typed, so a
/// caller tells this refusal from a failed persist (`anyhow::Error::is` /
/// `chain`).
#[derive(Debug, Clone)]
pub struct IndexRemovedDuringPersist {
    graph_dir: PathBuf,
}

impl IndexRemovedDuringPersist {
    /// The refusal for the index whose `.sqry/graph/` directory is
    /// `graph_dir`.
    #[must_use]
    pub fn new(graph_dir: &Path) -> Self {
        Self {
            graph_dir: graph_dir.to_path_buf(),
        }
    }
}

impl std::fmt::Display for IndexRemovedDuringPersist {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the index directory {} was removed during the persist; no index was written",
            self.graph_dir.display()
        )
    }
}

impl std::error::Error for IndexRemovedDuringPersist {}

/// How many times [`IndexWriteLock::acquire`] and `try_take` retry a lock
/// whose file the path no longer names after `flock` (decision D-i8-6).
/// One mismatch is a removal racing the lock; each retry reopens whatever
/// the path names now, so on a sane filesystem the second attempt already
/// matches unless the index is removed again in that instant. Eight
/// consecutive mismatches mean the identity itself is unstable (a network
/// or FUSE filesystem, an overlayfs copy-up): retrying cannot help, and
/// without a bound the loop would spin for ever.
pub const LOCK_IDENTITY_RETRIES: usize = 8;

/// The persist lock of `lock_path` cannot be held reliably: after
/// [`LOCK_IDENTITY_RETRIES`] attempts the locked file was never the one the
/// path names, as on a filesystem whose inode numbers are not stable.
/// Typed, so a caller can tell it from other failures.
#[derive(Debug, Clone)]
pub struct UnstableLockFile {
    lock_path: PathBuf,
}

impl std::fmt::Display for UnstableLockFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the persist lock {} has an unstable file identity: after {LOCK_IDENTITY_RETRIES} \
             attempts the locked file was never the one the path names (a filesystem whose \
             inode numbers are not stable?)",
            self.lock_path.display()
        )
    }
}

impl std::error::Error for UnstableLockFile {}

/// How a non-blocking attempt at the lock ended (`IndexWriteLock::try_take`).
/// Other open or lock failures are errors.
enum TryTake {
    /// A fresh hold on the lock file the path names after the `flock`.
    Taken(IndexWriteLock),
    /// This thread already holds the current lock file, or another holder
    /// has it (the non-blocking `flock` would block).
    Busy,
    /// `open_lock_file_in_index` answered `None`, or the lock file's
    /// identity never matched across [`LOCK_IDENTITY_RETRIES`] attempts
    /// (logged as [`UnstableLockFile`]).
    NoIndex,
}

/// How [`IndexWriteLock::acquire_unless_cancelled`] (or
/// [`IndexWriteLock::acquire_existing_unless_cancelled`]) ended. Only
/// `Held` carries a guard; the others release a lock that arrived, and
/// leave a hold the thread already had as it is.
#[derive(Debug)]
pub enum LockWait {
    /// The lock, with `cancelled()` false when it arrived (on a re-entry,
    /// a guard sharing the thread's hold).
    Held(IndexWriteLock),
    /// `cancelled()` answered true: on a re-entry, at a step, or when the
    /// lock arrived.
    Cancelled,
    /// The path no longer names the lock file the wait opened (at a step,
    /// on arrival, or the thread's hold on a re-entry), or, for the
    /// existing-index wait, `open_lock_file_in_index` answered `None`.
    IndexRemoved,
}

/// Which file a lock was taken on (device and inode on Unix), so a hold
/// on a lock file that was removed, and perhaps recreated, is told from a
/// hold on the file the path names now: an open file still locks after it
/// is unlinked, and that lock excludes nobody who opens the path again
/// (third audit, item 1). Elsewhere `file_id` gives no identity (the
/// platform's own, a volume serial and file index on Windows, is not
/// exposed by stable `std`), so a check there can only tell that some lock
/// file exists at the path; a removal and recreation between two checks
/// goes unnoticed (decision D-i8-6).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct LockFileIdentity(Option<(u64, u64)>);

impl LockFileIdentity {
    fn of(file: &File) -> Self {
        Self(file.metadata().ok().and_then(|metadata| file_id(&metadata)))
    }

    /// Whether `path` still names the file this identity was taken from.
    fn matches(&self, path: &Path) -> bool {
        match fs::metadata(path) {
            Ok(metadata) => match (&self.0, file_id(&metadata)) {
                (Some(ours), Some(now)) => *ours == now,
                // No identity on this platform: a lock file that exists.
                _ => true,
            },
            Err(_) => false,
        }
    }
}

#[cfg(unix)]
fn file_id(metadata: &fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn file_id(_metadata: &fs::Metadata) -> Option<(u64, u64)> {
    None
}

/// The mark every rollback name carries, so a leftover is recognised.
pub(crate) const ROLLBACK_MARK: &str = ".rollback-";

/// The marker of a transaction that has set the old pair aside and not yet
/// committed: `.txn-begun.rollback-<pid>-<secs>-<seq>`.
pub(crate) const MARKER_BEGUN: &str = ".txn-begun";

/// The marker of a transaction that reached its commit point and has not
/// yet removed its set-aside files.
pub(crate) const MARKER_COMMITTED: &str = ".txn-committed";

/// One hold of one lock file on this thread: the open file whose `flock`
/// is the hold. Shared by the guard that took it and every re-entrant
/// guard, and released when the last of them drops (Codex round-eight
/// review, item 1).
#[derive(Debug)]
struct HoldOwner {
    file: File,
}

impl Drop for HoldOwner {
    fn drop(&mut self) {
        // Closing the file releases the lock; unlock first so the release
        // does not wait for the close.
        let _ = self.file.unlock();
    }
}

/// One entry of this thread's hold record: the directory as the hold was
/// taken, the lock file's identity, and the hold itself (weak, so the
/// record never keeps a hold alive past its last guard).
struct HeldEntry {
    dir: PathBuf,
    identity: LockFileIdentity,
    owner: std::rc::Weak<HoldOwner>,
}

thread_local! {
    /// The holds this thread has, one entry per hold (not per guard).
    static HELD: RefCell<Vec<HeldEntry>> = const { RefCell::new(Vec::new()) };
}

/// An exclusive hold of one index's persist lock. Released on drop.
///
/// The hold is recorded per thread (that is what makes a second
/// [`IndexWriteLock::acquire`], or [`IndexWriteLock::reenter`], on the
/// holding thread re-entrant). Every guard of one hold shares it: the lock
/// stays held, and the record kept, until the last of them drops, in
/// either order. The guard must be dropped on the thread that took it. It is neither `Send`
/// nor `Sync`; moving it to another thread does not compile:
///
/// ```compile_fail
/// use sqry_core::graph::unified::persistence::IndexWriteLock;
/// let dir = std::env::temp_dir().join("sqry-index-write-lock-doctest/.sqry/graph");
/// let held = IndexWriteLock::acquire(&dir).unwrap();
/// std::thread::spawn(move || drop(held)).join().unwrap();
/// ```
///
/// Nor can a reference to it be shared with another thread:
///
/// ```compile_fail
/// use sqry_core::graph::unified::persistence::IndexWriteLock;
/// fn shared<T: Sync>(_: &T) {}
/// let dir = std::env::temp_dir().join("sqry-index-write-lock-doctest/.sqry/graph");
/// let held = IndexWriteLock::acquire(&dir).unwrap();
/// shared(&held);
/// ```
#[derive(Debug)]
pub struct IndexWriteLock {
    /// The hold this guard shares: the guard that took it and every
    /// re-entrant guard own it together, and the lock is released, and the
    /// thread's record of it removed, only when the last of them drops, in
    /// whatever order they drop.
    owner: std::rc::Rc<HoldOwner>,
    graph_dir: PathBuf,
    /// The lock file this hold is on.
    identity: LockFileIdentity,
    /// Whether a committed index was seen ([`holds_committed_index`]):
    /// when the hold was taken, and for a wait also when it opened the
    /// lock file or through a commit recorded on that file while it waited.
    saw_committed_index: bool,
    /// Pins the guard to the thread whose `HELD` entry it owns: a raw
    /// pointer is neither `Send` nor `Sync` (decision D-i8-3).
    _thread_bound: PhantomData<*const ()>,
}

impl IndexWriteLock {
    /// Take the persist lock of the index whose `.sqry/graph/` directory is
    /// `graph_dir`, blocking until no other holder has it. Creates the
    /// directory and the lock file if they do not exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory or the lock file cannot be
    /// created or opened, or the lock cannot be taken.
    pub fn acquire(graph_dir: &Path) -> Result<Self> {
        if let Some(held) = Self::reenter(graph_dir) {
            return Ok(held);
        }
        for _ in 0..LOCK_IDENTITY_RETRIES {
            fs::create_dir_all(graph_dir)
                .with_context(|| format!("Failed to create {}", graph_dir.display()))?;
            let file = open_lock_file(graph_dir)?;
            file.lock().with_context(|| {
                format!(
                    "Failed to take the persist lock {}",
                    graph_dir.join(PERSIST_LOCK_FILE_NAME).display()
                )
            })?;
            // A lock won on a file removed while this waited holds nothing
            // anyone else respects: take the one the path names now.
            if let Some(identity) = Self::still_named(&file, graph_dir) {
                return Ok(Self::held(file, graph_dir, identity).recovered());
            }
        }
        Err(UnstableLockFile {
            lock_path: graph_dir.join(PERSIST_LOCK_FILE_NAME),
        }
        .into())
    }

    /// [`Self::acquire`] when `graph_dir` exists, blocking until no other
    /// holder has it; `Ok(None)`, creating nothing, when it does not.
    ///
    /// For a caller that resolves recorded build inputs and later persists
    /// (decision D-i8-2): with the directory present it holds the lock from
    /// resolution through publication, so no other transaction can move the
    /// manifest aside or publish in between. With no directory there is no
    /// record to resolve and nothing is created, so a request refused during
    /// resolution still leaves an unindexed root untouched; such a caller
    /// takes the lock before it builds and checks again that no manifest
    /// appeared.
    ///
    /// # Errors
    ///
    /// As [`Self::acquire`].
    pub fn acquire_if_present(graph_dir: &Path) -> Result<Option<Self>> {
        if Self::current_hold(graph_dir).is_some() || graph_dir.is_dir() {
            return Self::acquire(graph_dir).map(Some);
        }
        Ok(None)
    }

    /// [`Self::acquire`] for a caller that must stay cancellable while
    /// another holder has the lock (decision D-i8-6).
    ///
    /// A re-entry (this thread holds the lock on the file the path names)
    /// calls `cancelled()` first, then shares the hold ([`Self::reenter`]),
    /// creating and opening nothing. Otherwise it creates `graph_dir` and
    /// the lock file if missing, tries the lock once, and if another holder
    /// has it runs `contended` once and waits through a blocking `flock` on
    /// a helper thread (one per lock file in the process, keyed by the
    /// file's identity on Unix and by the path as spelled elsewhere). The
    /// calling thread checks `cancelled()` and the lock file's identity
    /// every [`LOCK_POLL_INTERVAL`] and again when the lock arrives; the
    /// guard is made on the calling thread.
    ///
    /// # Errors
    ///
    /// The directory or the lock file cannot be created or opened, locking
    /// fails for a reason other than another holder, or the helper thread
    /// cannot be started. A re-entry returns no error.
    pub fn acquire_unless_cancelled(
        graph_dir: &Path,
        cancelled: &dyn Fn() -> bool,
        contended: &mut dyn FnMut(),
    ) -> Result<LockWait> {
        Self::wait_unless_cancelled(graph_dir, true, cancelled, contended)
    }

    /// [`Self::acquire_unless_cancelled`] for an index that had content
    /// when the caller looked (fourth audit, item 3). It does not create
    /// the directory: it opens the lock file through
    /// `open_lock_file_in_index` and answers [`LockWait::IndexRemoved`]
    /// when that answers `None`. A re-entry is answered as there.
    ///
    /// # Errors
    ///
    /// As [`Self::acquire_unless_cancelled`], except that no directory is
    /// created.
    pub fn acquire_existing_unless_cancelled(
        graph_dir: &Path,
        cancelled: &dyn Fn() -> bool,
        contended: &mut dyn FnMut(),
    ) -> Result<LockWait> {
        Self::wait_unless_cancelled(graph_dir, false, cancelled, contended)
    }

    fn wait_unless_cancelled(
        graph_dir: &Path,
        create: bool,
        cancelled: &dyn Fn() -> bool,
        contended: &mut dyn FnMut(),
    ) -> Result<LockWait> {
        // Re-entry (this thread already holds the lock) still answers the
        // cancellation, so `Held` always means it was checked (Codex
        // round-nine review, item 1). A `Cancelled` re-entry takes no guard,
        // so the original hold is untouched: its record and its lock stay.
        if Self::held_by_this_thread(graph_dir) {
            if cancelled() {
                return Ok(LockWait::Cancelled);
            }
            return Ok(Self::reenter(graph_dir).map_or(LockWait::IndexRemoved, LockWait::Held));
        }
        let file = if create {
            fs::create_dir_all(graph_dir)
                .with_context(|| format!("Failed to create {}", graph_dir.display()))?;
            open_lock_file(graph_dir)?
        } else {
            // Creates no directory, creates a lock file only into an index
            // that still has content, and takes none from a directory
            // without content (`open_lock_file_in_index`).
            match open_lock_file_in_index(graph_dir)? {
                Some(file) => file,
                None => return Ok(LockWait::IndexRemoved),
            }
        };
        let identity = LockFileIdentity::of(&file);
        let lock_path = graph_dir.join(PERSIST_LOCK_FILE_NAME);
        // What the wait sees as it opens the lock file: the commits recorded
        // on it so far, then whether a committed index is in place.
        let opened_generation = commit_generation(&file);
        let opened_committed = holds_committed_index(graph_dir);
        let arrived = |file: File| -> LockWait {
            if !identity.matches(&lock_path) {
                return LockWait::IndexRemoved;
            }
            let committed_while_waiting = commit_generation(&file) > opened_generation;
            let mut held = Self::held(file, graph_dir, identity.clone()).recovered();
            held.saw_committed_index |= opened_committed || committed_while_waiting;
            if cancelled() {
                drop(held);
                return LockWait::Cancelled;
            }
            LockWait::Held(held)
        };
        match file.try_lock() {
            Ok(()) => return Ok(arrived(file)),
            Err(fs::TryLockError::WouldBlock) => drop(file),
            Err(fs::TryLockError::Error(e)) => {
                return Err(e).with_context(|| {
                    format!("Failed to take the persist lock {}", lock_path.display())
                });
            }
        }
        contended();
        let key = SharedLockKey::of(&lock_path, &identity);
        loop {
            let Some(shared) = SharedLockWait::attach(&key, &lock_path, &identity)? else {
                return Ok(LockWait::IndexRemoved);
            };
            let mut state = shared.lock_state();
            loop {
                if let Some(answer) = state.answer.take() {
                    state.interested -= 1;
                    drop(state);
                    return match answer {
                        Ok(file) => Ok(arrived(file)),
                        Err(e) => Err(e).with_context(|| {
                            format!("Failed to take the persist lock {}", lock_path.display())
                        }),
                    };
                }
                if state.finished {
                    // Another waiter took the lock this helper won: wait
                    // again, with a new helper if none is waiting.
                    state.interested -= 1;
                    break;
                }
                state = shared
                    .answered
                    .wait_timeout(state, LOCK_POLL_INTERVAL)
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .0;
                let gave_up = if cancelled() {
                    Some(LockWait::Cancelled)
                } else if !identity.matches(&lock_path) {
                    Some(LockWait::IndexRemoved)
                } else {
                    None
                };
                if let Some(outcome) = gave_up {
                    state.interested -= 1;
                    // The last waiter to leave releases a lock that arrived
                    // for nobody.
                    if state.interested == 0 {
                        drop(state.answer.take());
                    }
                    return Ok(outcome);
                }
            }
        }
    }

    /// Take the persist lock without blocking, through `TryTake`:
    /// `Ok(None)` for `Busy` and `NoIndex`. It does not create the
    /// directory; it opens the lock file through `open_lock_file_in_index`.
    ///
    /// # Errors
    ///
    /// Returns an error if the lock file cannot be created (while the
    /// directory still exists) or opened, or locking fails for a reason
    /// other than another holder.
    pub fn try_acquire(graph_dir: &Path) -> Result<Option<Self>> {
        Ok(match Self::try_take(graph_dir)? {
            TryTake::Taken(held) => Some(held),
            TryTake::Busy | TryTake::NoIndex => None,
        })
    }

    /// [`Self::try_acquire`], telling "another holder has it" from "there is
    /// no index to lock", which `recover_for_reader` answers differently.
    fn try_take(graph_dir: &Path) -> Result<TryTake> {
        if Self::held_by_this_thread(graph_dir) {
            return Ok(TryTake::Busy);
        }
        for _ in 0..LOCK_IDENTITY_RETRIES {
            // `open_lock_file_in_index` answered no index.
            let Some(file) = open_lock_file_in_index(graph_dir)? else {
                return Ok(TryTake::NoIndex);
            };
            match file.try_lock() {
                Ok(()) => {
                    if let Some(identity) = Self::still_named(&file, graph_dir) {
                        return Ok(TryTake::Taken(
                            Self::held(file, graph_dir, identity).recovered(),
                        ));
                    }
                    // Locked a file removed since it was opened: try the
                    // one the path names now.
                }
                Err(fs::TryLockError::WouldBlock) => return Ok(TryTake::Busy),
                Err(fs::TryLockError::Error(e)) => {
                    return Err(e).with_context(|| {
                        format!(
                            "Failed to try the persist lock {}",
                            graph_dir.join(PERSIST_LOCK_FILE_NAME).display()
                        )
                    });
                }
            }
        }
        // The identity never held still (decision D-i8-6): no lock, and
        // `NoIndex` so `recover_for_reader` does not wait on it.
        log::warn!(
            "{}",
            UnstableLockFile {
                lock_path: graph_dir.join(PERSIST_LOCK_FILE_NAME),
            }
        );
        Ok(TryTake::NoIndex)
    }

    /// Whether this thread holds the persist lock for `graph_dir`.
    ///
    /// Only a hold on the lock file the path names now counts: a hold on a
    /// file that was removed (and perhaps recreated) since is stale and
    /// excludes nobody, so it does not make this thread's next acquire
    /// re-entrant.
    #[must_use]
    pub fn held_by_this_thread(graph_dir: &Path) -> bool {
        Self::current_hold(graph_dir).is_some()
    }

    /// Whether this thread holds a lock for `graph_dir` on a lock file the
    /// path no longer names: the index directory was removed after the
    /// hold was taken. A persist that finds this writes nothing (decision
    /// D-i8-6): it would recreate the index the user removed.
    #[must_use]
    pub fn held_stale_by_this_thread(graph_dir: &Path) -> bool {
        let lock_path = graph_dir.join(PERSIST_LOCK_FILE_NAME);
        HELD.with(|held| {
            held.borrow()
                .iter()
                .any(|entry| entry.dir == graph_dir && !entry.identity.matches(&lock_path))
        })
    }

    /// Whether this hold is on the lock file the path names now. `false`
    /// once the index directory was removed after the hold was taken: the
    /// hold then excludes nobody, and a caller about to write must not.
    #[must_use]
    pub fn is_current(&self) -> bool {
        self.identity
            .matches(&self.graph_dir.join(PERSIST_LOCK_FILE_NAME))
    }

    /// Whether this thread holds any lock for `graph_dir`, current or
    /// stale.
    #[must_use]
    pub fn held_any_by_this_thread(graph_dir: &Path) -> bool {
        HELD.with(|held| held.borrow().iter().any(|entry| entry.dir == graph_dir))
    }

    /// A re-entrant hold of `graph_dir` when this thread holds the current
    /// lock file; `None`, taking and creating nothing, otherwise. For a
    /// persist that runs under its caller's hold.
    #[must_use]
    pub fn reenter(graph_dir: &Path) -> Option<Self> {
        Self::current_hold(graph_dir).map(|(identity, owner)| Self {
            owner,
            graph_dir: graph_dir.to_path_buf(),
            identity,
            saw_committed_index: holds_committed_index(graph_dir),
            _thread_bound: PhantomData,
        })
    }

    /// The identity of this thread's hold on the lock file `graph_dir`
    /// names now, if it has one.
    fn current_hold(graph_dir: &Path) -> Option<(LockFileIdentity, std::rc::Rc<HoldOwner>)> {
        let lock_path = graph_dir.join(PERSIST_LOCK_FILE_NAME);
        // With a file identity (Unix), a hold is matched by the lock file
        // the path names, whatever spelling of the directory took it (a
        // relative path, a symlink); without one, by the path as spelled.
        HELD.with(|held| {
            held.borrow().iter().find_map(|entry| {
                ((entry.identity.0.is_some() || entry.dir == graph_dir)
                    && entry.identity.matches(&lock_path))
                .then(|| entry.owner.upgrade())
                .flatten()
                .map(|owner| (entry.identity.clone(), owner))
            })
        })
    }

    /// The identity of `file`, when `graph_dir`'s lock path still names it.
    fn still_named(file: &File, graph_dir: &Path) -> Option<LockFileIdentity> {
        #[cfg(test)]
        if identity_seam::unstable(graph_dir) {
            return None;
        }
        let identity = LockFileIdentity::of(file);
        identity
            .matches(&graph_dir.join(PERSIST_LOCK_FILE_NAME))
            .then_some(identity)
    }

    /// A fresh hold first puts back any pair an interrupted persist left
    /// set aside ([`recover_interrupted_persist`]): no transaction is
    /// running, so every rollback set is a leftover, and every holder
    /// starts from the previous complete pair. Then it records whether a
    /// committed index is in place.
    fn recovered(mut self) -> Self {
        if let Err(e) = recover_interrupted_persist(&self.graph_dir) {
            log::error!(
                "could not put back the index an interrupted persist left in {}: {e:#}",
                self.graph_dir.display()
            );
        }
        self.saw_committed_index = holds_committed_index(&self.graph_dir);
        self
    }

    /// Whether this hold saw a committed index ([`holds_committed_index`]):
    /// when it was taken (after its recovery) or re-entered, and for
    /// [`Self::acquire_unless_cancelled`] and
    /// [`Self::acquire_existing_unless_cancelled`] also when the wait
    /// opened the lock file, or through a commit recorded on that file
    /// ([`Self::record_commit`]) between that open and the hold.
    #[must_use]
    pub fn saw_committed_index(&self) -> bool {
        self.saw_committed_index
    }

    /// Record a commit on the lock file this hold is on: the file grows by
    /// one byte (its content is never read; nothing shortens it before the
    /// directory is removed). A wait compares the length
    /// when it opened the file with the length when its hold arrived.
    /// Called by the persist transaction after it wrote the manifest.
    ///
    /// # Errors
    ///
    /// The file's length cannot be read or set.
    pub fn record_commit(&self) -> std::io::Result<()> {
        let len = self.owner.file.metadata()?.len();
        self.owner.file.set_len(len.saturating_add(1))
    }

    fn held(file: File, graph_dir: &Path, identity: LockFileIdentity) -> Self {
        let owner = std::rc::Rc::new(HoldOwner { file });
        HELD.with(|held| {
            held.borrow_mut().push(HeldEntry {
                dir: graph_dir.to_path_buf(),
                identity: identity.clone(),
                owner: std::rc::Rc::downgrade(&owner),
            });
        });
        Self {
            owner,
            graph_dir: graph_dir.to_path_buf(),
            identity,
            saw_committed_index: false,
            _thread_bound: PhantomData,
        }
    }
}

/// The commits recorded on an open lock file ([`IndexWriteLock::record_commit`]):
/// its length, or 0 when it cannot be read.
fn commit_generation(file: &File) -> u64 {
    file.metadata().map_or(0, |metadata| metadata.len())
}

impl Drop for IndexWriteLock {
    fn drop(&mut self) {
        // The last guard of this hold: remove the thread's record of it.
        // The hold itself (`HoldOwner`) drops with this guard's `owner`
        // right after, releasing the lock.
        if std::rc::Rc::strong_count(&self.owner) == 1 {
            let owner = std::rc::Rc::downgrade(&self.owner);
            HELD.with(|held| {
                held.borrow_mut()
                    .retain(|entry| !std::rc::Weak::ptr_eq(&entry.owner, &owner));
            });
        }
    }
}

/// One helper thread's blocking wait for one lock file, shared by every
/// cancellable wait in the process that wants that file (third audit, item
/// 3): at most one helper per lock file, so cancelled waits do not pile up
/// threads and open files while another holder keeps the lock.
#[derive(Default)]
struct SharedLockWait {
    state: Mutex<SharedLockState>,
    answered: Condvar,
}

#[derive(Default)]
struct SharedLockState {
    /// Waits attached and not yet gone.
    interested: usize,
    /// The helper's answer, for the first waiter to take.
    answer: Option<std::io::Result<File>>,
    /// The helper has answered (and left the registry).
    finished: bool,
}

/// Which lock file a helper waits for: its identity on Unix, whatever path
/// spelling a wait used (Codex round-eight review, item 2: keyed by path
/// too, 16 symlink aliases of one directory left 16 helpers parked); the
/// path as spelled where there is no identity, so off Unix the bound is
/// one helper per lock file per spelling.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SharedLockKey {
    path: Option<PathBuf>,
    identity: LockFileIdentity,
}

impl SharedLockKey {
    fn of(lock_path: &Path, identity: &LockFileIdentity) -> Self {
        Self {
            path: identity.0.is_none().then(|| lock_path.to_path_buf()),
            identity: identity.clone(),
        }
    }
}

/// The helpers waiting now, by lock file. Lock order: this registry, then a
/// helper's state.
fn shared_lock_waits()
-> &'static Mutex<std::collections::HashMap<SharedLockKey, Arc<SharedLockWait>>> {
    static WAITS: OnceLock<Mutex<std::collections::HashMap<SharedLockKey, Arc<SharedLockWait>>>> =
        OnceLock::new();
    WAITS.get_or_init(Default::default)
}

/// Test seams for the helper's failure paths (fourth audit, item 1): the
/// next helper for one lock file fails to start, or panics when it starts.
#[cfg(test)]
pub(crate) mod helper_seam {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Fault {
        SpawnFails,
        Panics,
    }

    /// Armed faults, one per lock file, so tests running at once do not
    /// see each other's.
    static ARMED: Mutex<Vec<(PathBuf, Fault)>> = Mutex::new(Vec::new());

    pub(crate) fn arm(lock_path: &Path, fault: Fault) {
        ARMED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((lock_path.to_path_buf(), fault));
    }

    /// The fault armed for `lock_path`, consumed.
    pub(crate) fn take(lock_path: &Path) -> Option<Fault> {
        let mut armed = ARMED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let at = armed.iter().position(|(path, _)| path == lock_path)?;
        Some(armed.remove(at).1)
    }
}

impl SharedLockWait {
    fn lock_state(&self) -> std::sync::MutexGuard<'_, SharedLockState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Attach to the helper waiting for `key`'s lock file, starting one if
    /// none is. `Ok(None)` when the path no longer names that file.
    fn attach(
        key: &SharedLockKey,
        lock_path: &Path,
        identity: &LockFileIdentity,
    ) -> Result<Option<Arc<Self>>> {
        let mut waits = shared_lock_waits()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(shared) = waits.get(key) {
            let mut state = shared.lock_state();
            if !state.finished {
                state.interested += 1;
                drop(state);
                return Ok(Some(Arc::clone(shared)));
            }
        }
        // The path must still name the file the wait started on.
        if !identity.matches(lock_path) {
            return Ok(None);
        }
        // Open the existing file only; this path creates none.
        let file = match OpenOptions::new().read(true).write(true).open(lock_path) {
            Ok(file) => file,
            Err(_) if !identity.matches(lock_path) => return Ok(None),
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("Failed to open the persist lock {}", lock_path.display())
                });
            }
        };
        if LockFileIdentity::of(&file) != *identity {
            return Ok(None);
        }
        let shared = Arc::new(Self::default());
        shared.lock_state().interested = 1;
        let (helper_shared, helper_key) = (Arc::clone(&shared), key.clone());
        #[cfg(test)]
        let fault = helper_seam::take(lock_path);
        #[cfg(test)]
        if fault == Some(helper_seam::Fault::SpawnFails) {
            return Err(anyhow::anyhow!("seam: the helper thread could not start"))
                .context("Failed to start the persist lock waiter");
        }
        // Registered only once the helper runs (the registry lock is still
        // held, so it cannot answer before it is registered): a helper that
        // never started leaves no entry for later waits to attach to.
        std::thread::Builder::new()
            .name("sqry-persist-lock-wait".to_string())
            .spawn(move || {
                // Made on the helper thread (a failed spawn drops only the
                // two handles, not this, so it never locks the registry the
                // attaching thread holds). It answers when dropped, so a
                // panic below still finishes the entry and wakes its
                // waiters.
                let mut helper = HelperExit {
                    shared: helper_shared,
                    key: helper_key,
                    answer: None,
                };
                #[cfg(test)]
                if fault == Some(helper_seam::Fault::Panics) {
                    panic!("seam: the persist lock helper panicked");
                }
                helper.answer = Some(file.lock().map(|()| file));
            })
            .context("Failed to start the persist lock waiter")?;
        waits.insert(key.clone(), Arc::clone(&shared));
        Ok(Some(shared))
    }
}

/// A helper's way out, on every path (fourth audit, item 1): it leaves the
/// registry and marks its entry finished, handing its answer to a waiter
/// still there or, with none, dropping it (releasing the lock). Run by
/// `Drop`, so a helper that panics still finishes its entry: its waiters
/// then wait again with a new helper instead of on a dead one.
struct HelperExit {
    shared: Arc<SharedLockWait>,
    key: SharedLockKey,
    answer: Option<std::io::Result<File>>,
}

impl Drop for HelperExit {
    fn drop(&mut self) {
        let mut waits = shared_lock_waits()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if waits
            .get(&self.key)
            .is_some_and(|current| Arc::ptr_eq(current, &self.shared))
        {
            waits.remove(&self.key);
        }
        let mut state = self.shared.lock_state();
        state.finished = true;
        if state.interested > 0 {
            state.answer = self.answer.take();
        }
        self.shared.answered.notify_all();
        // With no waiter left, the answer drops with `self`, releasing the
        // lock at once.
    }
}

/// Whether `graph_dir` holds index content: `manifest.json`,
/// `snapshot.sqry`, or a rollback name. The existing-index lock paths'
/// rule (`open_lock_file_in_index`): a first persist's leftover counts, so
/// a reader's recovery can take the lock over it.
#[must_use]
pub fn holds_index_content(graph_dir: &Path) -> bool {
    graph_dir.join("manifest.json").exists()
        || graph_dir.join("snapshot.sqry").exists()
        || has_rollback_names(graph_dir)
}

/// How many times [`holds_committed_index`] reads the directory again when
/// the manifest or the snapshot changed during a read.
const COMMITTED_READ_ATTEMPTS: usize = 16;

/// Whether `graph_dir` holds a committed index: a manifest or a snapshot
/// is left once [`recover_interrupted_persist`] has run over the rollback
/// sets present, as it decides them (`RollbackSet::fate`). A first
/// persist's leftover (no previous pair) is no committed index; a set that
/// holds a previous pair aside is.
///
/// Under the persist lock nothing else changes these files (the
/// transaction and recovery run under it), so one read is consistent.
/// Without it, the manifest and the snapshot are opened and their device
/// and inode taken before the listing of the rollback names (which also
/// reads each begun marker), and the paths' device and inode taken after
/// it; an answer is taken only when
/// both are unchanged, and after 16 reads that changed
/// (`COMMITTED_READ_ATTEMPTS`) the last answer is taken. The premise: the
/// files stay open until the comparison, so their inodes cannot be freed
/// and their numbers cannot name a new file; an unchanged pair then is the
/// same file under the same name, present across the listing, and a
/// transaction writes its marker before its snapshot and removes the
/// snapshot first. A file renamed away and back in between (a failed
/// update's rollback and recovery's restore put the set-aside pair back
/// under its own inode) reads as unchanged; it is the previous committed
/// file, so true is right. Off Unix, or where a file is not a regular file
/// it can open, any replacement goes unseen (only presence, or the path's
/// identity, is compared).
#[must_use]
pub fn holds_committed_index(graph_dir: &Path) -> bool {
    let manifest_path = graph_dir.join(super::MANIFEST_FILE_NAME);
    let snapshot_path = graph_dir.join(super::SNAPSHOT_FILE_NAME);
    let pair = || (presence(&manifest_path), presence(&snapshot_path));
    let mut answer = false;
    for _ in 0..COMMITTED_READ_ATTEMPTS {
        let before = pair();
        #[cfg(any(test, feature = "test-support"))]
        committed_read_seam::run(graph_dir, committed_read_seam::Point::First);
        let listing = RollbackListing::of(graph_dir);
        #[cfg(any(test, feature = "test-support"))]
        committed_read_seam::run(graph_dir, committed_read_seam::Point::Second);
        let after = pair();
        #[cfg(any(test, feature = "test-support"))]
        committed_read_seam::run(graph_dir, committed_read_seam::Point::Third);
        answer = listing.leaves_an_index(after.0.is_some(), after.1.is_some());
        // `before` still holds its files open here.
        if pair_ids(&before) == pair_ids(&after) {
            return answer;
        }
    }
    answer
}

/// A file `holds_committed_index` saw: its identity (none off Unix, where
/// only its presence is compared) and, when it could be opened, the open
/// file that keeps that identity from being reused while the read runs.
struct Seen {
    id: Option<(u64, u64)>,
    _open: Option<File>,
}

/// `None` when nothing is at `path`. A regular file is held open; anything
/// else there (a directory, a FIFO, a socket, a device) counts as present,
/// as recovery's `exists()` counts it, and has only the path's identity.
fn presence(path: &Path) -> Option<Seen> {
    if let Some(file) = open_regular(path) {
        return Some(Seen {
            id: file.metadata().ok().and_then(|metadata| file_id(&metadata)),
            _open: Some(file),
        });
    }
    fs::metadata(path).ok().map(|metadata| Seen {
        id: file_id(&metadata),
        _open: None,
    })
}

/// `path` opened for reading when it is a regular file, `None` otherwise.
/// On Unix the open does not block (`O_NONBLOCK`: opening a FIFO for
/// reading otherwise waits for a writer) and the opened file must be a
/// regular file.
fn open_regular(path: &Path) -> Option<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).ok()?;
    file.metadata().ok()?.is_file().then_some(file)
}

/// The identities of a manifest and snapshot pair, for comparing.
fn pair_ids(pair: &(Option<Seen>, Option<Seen>)) -> [Option<Option<(u64, u64)>>; 2] {
    [
        pair.0.as_ref().map(|seen| seen.id),
        pair.1.as_ref().map(|seen| seen.id),
    ]
}

/// The lock file of an existing index, opened without creating the
/// directory, or `None` (fifth audit, item 1; decision D-i8-6). A present
/// lock file is opened, and `None` is answered if `holds_index_content` is
/// then false. A missing one is created with `create_new` only when the
/// directory exists and the content check passes; if the check after the
/// creation fails, `None` is answered and the file is left (removing it
/// could remove another writer's newer one). On `AlreadyExists` the file is
/// opened as a present one, or `None` if it is gone before the open.
fn open_lock_file_in_index(graph_dir: &Path) -> Result<Option<File>> {
    let path = graph_dir.join(PERSIST_LOCK_FILE_NAME);
    let file = match OpenOptions::new().read(true).write(true).open(&path) {
        // Present at entry: not this call's; this function does not remove it.
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return create_lock_file_in_index(graph_dir, &path);
        }
        Err(e) => {
            return Err(e)
                .with_context(|| format!("Failed to open the persist lock {}", path.display()));
        }
    };
    // The content check runs after the open (owner decision, round nine).
    Ok(holds_index_content(graph_dir).then_some(file))
}

/// The missing lock file of `graph_dir`, created exclusively into an index
/// that still has content (`open_lock_file_in_index`).
fn create_lock_file_in_index(graph_dir: &Path, path: &Path) -> Result<Option<File>> {
    if !graph_dir.is_dir() || !holds_index_content(graph_dir) {
        return Ok(None);
    }
    #[cfg(test)]
    lock_create_seam::before_create(graph_dir);
    // An exclusive create, so this call knows whether the file is its own.
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(file) => file,
        // Another opener's file: opened as a present one; gone, no index.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            #[cfg(test)]
            lock_create_seam::after_already_exists(graph_dir);
            return match OpenOptions::new().read(true).write(true).open(path) {
                Ok(file) => Ok(holds_index_content(graph_dir).then_some(file)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e)
                    .with_context(|| format!("Failed to open the persist lock {}", path.display())),
            };
        }
        Err(_) if !graph_dir.is_dir() => return Ok(None),
        Err(e) => {
            return Err(e)
                .with_context(|| format!("Failed to create the persist lock {}", path.display()));
        }
    };
    #[cfg(test)]
    lock_create_seam::after_create(graph_dir);
    // The created file is left even when the content is gone: removing it
    // could remove a lock file another writer has since recreated.
    Ok(holds_index_content(graph_dir).then_some(file))
}

/// Test seam: run a closure at one of the points inside a
/// `holds_committed_index` read, for one graph directory, once: `First`
/// after its first look at the manifest and snapshot, `Second` after its
/// listing of the rollback names, `Third` after its last look at the
/// manifest and snapshot. Compiled for this crate's tests and, under the
/// `test-support` feature, for other crates' tests.
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod committed_read_seam {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    /// Where in the read the closure runs.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Point {
        /// After the first look at the manifest and snapshot.
        First,
        /// After the listing of the rollback names.
        Second,
        /// After the last look at the manifest and snapshot.
        Third,
    }

    type Action = Box<dyn FnOnce() + Send>;

    static ARMED: Mutex<Vec<(PathBuf, Point, Action)>> = Mutex::new(Vec::new());

    /// Run `action` once, at `point` of the next read of `graph_dir`.
    pub fn arm(graph_dir: &Path, point: Point, action: Action) {
        ARMED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((graph_dir.to_path_buf(), point, action));
    }

    pub(crate) fn run(graph_dir: &Path, point: Point) {
        let action = {
            let mut armed = ARMED
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            armed
                .iter()
                .position(|(dir, at, _)| dir == graph_dir && *at == point)
                .map(|at| armed.remove(at).2)
        };
        if let Some(action) = action {
            action();
        }
    }
}

/// Test seam: make the lock file identity check fail for one graph
/// directory, as a filesystem with unstable inode numbers would (the bound
/// on the identity retries, decision D-i8-6).
#[cfg(test)]
pub(crate) mod identity_seam {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    static UNSTABLE: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

    pub(crate) fn arm(graph_dir: &Path) {
        UNSTABLE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(graph_dir.to_path_buf());
    }

    pub(crate) fn unstable(graph_dir: &Path) -> bool {
        UNSTABLE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|dir| dir == graph_dir)
    }
}

/// Test seam: run a closure right after `open_lock_file_in_index` created a
/// missing lock file (fifth audit, item 1), or right before it creates one,
/// after the first open found none (the window in which another opener can
/// create it, round-nine verification), for one graph directory.
#[cfg(test)]
pub(crate) mod lock_create_seam {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    type Action = Box<dyn FnOnce() + Send>;

    static ARMED: Mutex<Vec<(PathBuf, Action)>> = Mutex::new(Vec::new());
    static ARMED_BEFORE: Mutex<Vec<(PathBuf, Action)>> = Mutex::new(Vec::new());
    static ARMED_AFTER_EXISTS: Mutex<Vec<(PathBuf, Action)>> = Mutex::new(Vec::new());

    pub(crate) fn arm(graph_dir: &Path, action: Action) {
        ARMED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((graph_dir.to_path_buf(), action));
    }

    pub(crate) fn arm_before(graph_dir: &Path, action: Action) {
        ARMED_BEFORE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((graph_dir.to_path_buf(), action));
    }

    fn run(armed: &Mutex<Vec<(PathBuf, Action)>>, graph_dir: &Path) {
        let action = {
            let mut armed = armed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            armed
                .iter()
                .position(|(dir, _)| dir == graph_dir)
                .map(|at| armed.remove(at).1)
        };
        if let Some(action) = action {
            action();
        }
    }

    pub(crate) fn after_create(graph_dir: &Path) {
        run(&ARMED, graph_dir);
    }

    pub(crate) fn before_create(graph_dir: &Path) {
        run(&ARMED_BEFORE, graph_dir);
    }

    /// Arm a closure to run after the exclusive create found the file
    /// already there and before it is opened as another opener's.
    pub(crate) fn arm_after_already_exists(graph_dir: &Path, action: Action) {
        ARMED_AFTER_EXISTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((graph_dir.to_path_buf(), action));
    }

    pub(crate) fn after_already_exists(graph_dir: &Path) {
        run(&ARMED_AFTER_EXISTS, graph_dir);
    }
}

fn open_lock_file(graph_dir: &Path) -> Result<File> {
    let path = graph_dir.join(PERSIST_LOCK_FILE_NAME);
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("Failed to open the persist lock {}", path.display()))
}

/// What [`recover_interrupted_persist`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RecoveryOutcome {
    /// Rollback sets whose previous pair was put back.
    pub restored: usize,
    /// Rollback sets of committed (or never-started) transactions, removed.
    pub discarded: usize,
}

/// Put back the previous complete manifest and snapshot pair a crashed or
/// killed persist left under its rollback names, and remove the rollback
/// sets of transactions that committed. The caller must hold the persist
/// lock of `graph_dir` ([`IndexWriteLock`]): every set it finds then
/// belongs to a transaction that is no longer running.
///
/// A set is committed when its marker says so or when `manifest.json`
/// exists (the transaction moved the old manifest aside before anything
/// else, so a manifest present now is the one it wrote at its commit
/// point). Otherwise its snapshot is put back first and its manifest only
/// once the snapshot is back, so the manifest never reappears beside a
/// snapshot it does not describe. Sets are processed oldest first; a set
/// that cannot be put back stops the recovery and stays in place.
///
/// # Errors
///
/// Returns an error if a set-aside file cannot be put back.
pub fn recover_interrupted_persist(graph_dir: &Path) -> Result<RecoveryOutcome> {
    let mut outcome = RecoveryOutcome::default();
    let listing = RollbackListing::of(graph_dir);
    let manifest_path = graph_dir.join(super::MANIFEST_FILE_NAME);
    let snapshot_path = graph_dir.join(super::SNAPSHOT_FILE_NAME);
    for set in listing.sets.into_values() {
        let (snapshot, manifest_back) = match set.fate(manifest_path.exists()) {
            SetFate::Discard => {
                set.discard();
                outcome.discarded += 1;
                continue;
            }
            SetFate::Restore {
                snapshot,
                manifest_back,
            } => (snapshot, manifest_back),
        };
        match (snapshot, &set.snapshot_aside) {
            (SnapshotRestore::PutBack, Some(aside)) => {
                fs::rename(aside, &snapshot_path).with_context(|| {
                    format!(
                        "Failed to put the previous snapshot back from {}",
                        aside.display()
                    )
                })?;
            }
            (SnapshotRestore::RemoveNew, _) => match fs::remove_file(&snapshot_path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(e).with_context(|| {
                        format!(
                            "Failed to remove the snapshot {} an interrupted persist wrote",
                            snapshot_path.display()
                        )
                    });
                }
            },
            _ => {}
        }
        if manifest_back && let Some(aside) = &set.manifest_aside {
            fs::rename(aside, &manifest_path).with_context(|| {
                format!(
                    "Failed to put the previous manifest back from {}",
                    aside.display()
                )
            })?;
        }
        log::warn!(
            "put back the index pair an interrupted persist set aside in {}",
            graph_dir.display()
        );
        set.discard();
        outcome.restored += 1;
    }
    Ok(outcome)
}

/// The rollback sets in a graph directory, oldest first.
#[derive(Debug, Default)]
struct RollbackListing {
    sets: std::collections::BTreeMap<(u64, u64, u64), RollbackSet>,
}

impl RollbackListing {
    /// Empty when the directory cannot be read.
    fn of(graph_dir: &Path) -> Self {
        let mut listing = Self::default();
        let Ok(entries) = fs::read_dir(graph_dir) else {
            return listing;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(at) = name.find(ROLLBACK_MARK) else {
                continue;
            };
            let Some(order) = parse_tag(&name[at + ROLLBACK_MARK.len()..]) else {
                continue;
            };
            let set = listing.sets.entry(order).or_default();
            let path = entry.path();
            match &name[..at] {
                ".manifest.json" => set.manifest_aside = Some(path),
                ".snapshot.sqry" => set.snapshot_aside = Some(path),
                MARKER_BEGUN => {
                    // Read now, during the listing, so a read without the
                    // lock checks it inside its identity interval.
                    set.begun_without_snapshot = marker_says_no_snapshot(&path);
                    set.begun = Some(path);
                }
                MARKER_COMMITTED => set.committed = Some(path),
                _ => set.other.push(path),
            }
        }
        listing
    }

    /// Whether a recovery over these sets, starting from a manifest and a
    /// snapshot present as given, leaves a manifest or a snapshot.
    fn leaves_an_index(&self, mut manifest: bool, mut snapshot: bool) -> bool {
        for set in self.sets.values() {
            if let SetFate::Restore {
                snapshot: restore,
                manifest_back,
            } = set.fate(manifest)
            {
                match restore {
                    SnapshotRestore::PutBack => snapshot = true,
                    SnapshotRestore::RemoveNew => snapshot = false,
                    SnapshotRestore::Keep => {}
                }
                manifest |= manifest_back;
            }
        }
        manifest || snapshot
    }
}

/// What recovery does with one rollback set ([`RollbackSet::fate`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetFate {
    /// The transaction committed: the set is removed.
    Discard,
    /// The transaction did not commit: the previous pair goes back.
    Restore {
        snapshot: SnapshotRestore,
        /// The previous manifest is put back (it was set aside).
        manifest_back: bool,
    },
}

/// What a restore does with `snapshot.sqry`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapshotRestore {
    /// The previous snapshot, set aside, is put back.
    PutBack,
    /// The transaction began with no snapshot: the one present is its own
    /// and is removed.
    RemoveNew,
    /// Left as it is.
    Keep,
}

/// [`recover_interrupted_persist`] for a reader, when the manifest is
/// missing and a rollback name is present and this thread holds no lock
/// for `graph_dir` (decision D-i8-1). `Taken` recovers as the hold is
/// taken; `Busy` waits through the existing-index wait and recovers as
/// that hold is taken; `NoIndex` returns. Failures are logged.
pub(crate) fn recover_for_reader(graph_dir: &Path, manifest_path: &Path) {
    if manifest_path.exists()
        || IndexWriteLock::held_by_this_thread(graph_dir)
        || !has_rollback_names(graph_dir)
    {
        return;
    }
    let held = match IndexWriteLock::try_take(graph_dir) {
        // A fresh hold recovers as it is taken.
        Ok(TryTake::Taken(_held)) => Ok(()),
        // Another holder: wait through the existing-index wait, which does
        // not create the directory (decision D-i8-6).
        Ok(TryTake::Busy) => {
            IndexWriteLock::acquire_existing_unless_cancelled(graph_dir, &|| false, &mut || {})
                .map(drop)
        }
        // No index to lock: nothing to recover.
        Ok(TryTake::NoIndex) => Ok(()),
        Err(e) => Err(e),
    };
    if let Err(e) = held {
        log::warn!(
            "could not check {} for an interrupted persist: {e:#}",
            graph_dir.display()
        );
    }
}

fn has_rollback_names(graph_dir: &Path) -> bool {
    fs::read_dir(graph_dir).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().contains(ROLLBACK_MARK))
    })
}

/// `<pid>-<secs>-<seq>` to an order: time, then pid, then sequence.
fn parse_tag(tag: &str) -> Option<(u64, u64, u64)> {
    let mut parts = tag.split('-');
    let pid = parts.next()?.parse().ok()?;
    let secs = parts.next()?.parse().ok()?;
    let seq = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((secs, pid, seq))
}

/// The begun marker records `snapshot_existed=0` when the transaction
/// began with no snapshot, so a snapshot present now is one it wrote. A
/// marker that is not a readable regular file says nothing
/// (`open_regular`, so a FIFO there does not block).
fn marker_says_no_snapshot(marker: &Path) -> bool {
    use std::io::Read;
    let mut text = String::new();
    open_regular(marker).is_some_and(|mut file| file.read_to_string(&mut text).is_ok())
        && text.contains("snapshot_existed=0")
}

#[derive(Debug, Default)]
struct RollbackSet {
    manifest_aside: Option<PathBuf>,
    snapshot_aside: Option<PathBuf>,
    begun: Option<PathBuf>,
    /// The begun marker said `snapshot_existed=0` when the set was listed.
    begun_without_snapshot: bool,
    committed: Option<PathBuf>,
    other: Vec<PathBuf>,
}

impl RollbackSet {
    /// What recovery does with this set when a manifest is (or is not) in
    /// place as it gets to it. A set is committed when its marker says so
    /// or when `manifest.json` exists (the transaction moved the old
    /// manifest aside before anything else, so a manifest present now is
    /// the one it wrote at its commit point).
    fn fate(&self, manifest_present: bool) -> SetFate {
        if self.committed.is_some() || manifest_present {
            return SetFate::Discard;
        }
        let snapshot = if self.snapshot_aside.is_some() {
            SnapshotRestore::PutBack
        } else if self.begun_without_snapshot {
            SnapshotRestore::RemoveNew
        } else {
            SnapshotRestore::Keep
        };
        SetFate::Restore {
            snapshot,
            manifest_back: self.manifest_aside.is_some(),
        }
    }

    /// Remove the set's files: the set-aside files first, the markers last.
    fn discard(self) {
        for path in [self.manifest_aside, self.snapshot_aside]
            .into_iter()
            .flatten()
            .chain(self.other)
            .chain([self.begun, self.committed].into_iter().flatten())
        {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => log::warn!("could not remove {}: {e}", path.display()),
            }
        }
    }
}

/// The process-wide persist gate ([`close_persists_and_wait`]).
#[derive(Debug, Default)]
pub struct PersistGate {
    state: Mutex<GateState>,
    idle: Condvar,
}

#[derive(Debug, Default)]
struct GateState {
    closed: bool,
    in_flight: usize,
}

/// A persist in flight through a [`PersistGate`]; leaves the gate on drop.
#[derive(Debug)]
pub struct PersistTicket<'a> {
    gate: &'a PersistGate,
}

impl PersistGate {
    /// A new, open gate.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The gate every durable persist of this process passes.
    pub fn global() -> &'static Self {
        static GATE: OnceLock<PersistGate> = OnceLock::new();
        GATE.get_or_init(PersistGate::new)
    }

    /// Enter the gate for one persist.
    ///
    /// # Errors
    ///
    /// Refuses once the gate is closed: the process is exiting, and a
    /// persist that has not begun must not begin.
    pub fn enter(&self) -> Result<PersistTicket<'_>> {
        let mut state = self.lock();
        if state.closed {
            bail!("the process is shutting down; the persist was not started");
        }
        state.in_flight += 1;
        Ok(PersistTicket { gate: self })
    }

    /// Close the gate, so a later `enter` is refused, and wait without a
    /// bound until every persist that entered has dropped its ticket.
    pub fn close_and_wait(&self) {
        let mut state = self.lock();
        state.closed = true;
        while state.in_flight > 0 {
            state = self
                .idle
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// Whether the gate is closed (the process is exiting), so a wait that
    /// would end in a refused persist can end now.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.lock().closed
    }

    /// How many persists are in flight.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.lock().in_flight
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, GateState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Drop for PersistTicket<'_> {
    fn drop(&mut self) {
        let mut state = self.gate.lock();
        state.in_flight -= 1;
        if state.in_flight == 0 {
            self.gate.idle.notify_all();
        }
    }
}

/// Close this process's persist gate and wait for every durable persist in
/// flight to finish ([`PersistGate::close_and_wait`] on
/// [`PersistGate::global`]). For a process that is exiting.
pub fn close_persists_and_wait() {
    PersistGate::global().close_and_wait();
}

/// Test hook: a closure every durable persist runs once it has set the old
/// pair aside, before it writes the new snapshot. Lets a process-level test
/// stop or kill a daemon in the middle of a persist. Set at most once.
#[doc(hidden)]
pub fn set_mid_persist_hook(hook: fn(&Path)) {
    let _ = MID_PERSIST_HOOK.set(hook);
}

static MID_PERSIST_HOOK: OnceLock<fn(&Path)> = OnceLock::new();

pub(crate) fn run_mid_persist_hook(graph_dir: &Path) {
    if let Some(hook) = MID_PERSIST_HOOK.get() {
        hook(graph_dir);
    }
}

/// Test support: wait on the kernel's own record that a thread is blocked
/// on an index's persist lock, instead of a fixed sleep (Linux only:
/// `/proc/locks`).
#[cfg(all(test, target_os = "linux"))]
pub(crate) mod lock_waiters {
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;
    use std::time::{Duration, Instant};

    /// Whether some thread waits for the persist lock of `graph_dir` now:
    /// a `->` waiter line in `/proc/locks` naming the lock file's device
    /// (major and minor, hex) and inode. The device is the one `stat`
    /// reports; where that differs from the superblock's (btrfs
    /// subvolumes) nothing matches and the caller's deadline fails.
    pub(crate) fn someone_waits(graph_dir: &Path) -> bool {
        // No lock file at the path: nobody can be waiting on it.
        let Ok(metadata) = std::fs::metadata(graph_dir.join(super::PERSIST_LOCK_FILE_NAME)) else {
            return false;
        };
        let dev = metadata.dev();
        let wanted = (
            ((dev >> 8) & 0xfff) | ((dev >> 32) & 0xffff_f000),
            (dev & 0xff) | ((dev >> 12) & 0xffff_ff00),
            metadata.ino(),
        );
        let locks = std::fs::read_to_string("/proc/locks").expect("read /proc/locks");
        locks.lines().any(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            fields
                .iter()
                .position(|field| *field == "->")
                .and_then(|at| fields.get(at + 5))
                .and_then(|id| {
                    let mut parts = id.split(':');
                    let major = u64::from_str_radix(parts.next()?, 16).ok()?;
                    let minor = u64::from_str_radix(parts.next()?, 16).ok()?;
                    let ino = parts.next()?.parse::<u64>().ok()?;
                    parts.next().is_none().then_some((major, minor, ino))
                })
                .is_some_and(|waiter| waiter == wanted)
        })
    }

    /// Wait until a thread waits for the persist lock of `graph_dir`
    /// (`true`), or until `gave_up` holds first (`false`), which a caller
    /// uses for "the thread finished instead of waiting".
    pub(crate) fn wait_until_someone_waits(graph_dir: &Path, gave_up: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if someone_waits(graph_dir) {
                return true;
            }
            if gave_up() {
                return false;
            }
            assert!(
                Instant::now() < deadline,
                "no thread waited for the persist lock of {}",
                graph_dir.display()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    #[test]
    fn the_lock_excludes_another_thread_and_is_reentrant_on_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let graph_dir = dir.path().join(".sqry/graph");
        let held = IndexWriteLock::acquire(&graph_dir).unwrap();
        // Index content, so the probes below can take the lock at all.
        fs::write(graph_dir.join("manifest.json"), b"{}").unwrap();
        // Re-entrant on the holding thread.
        let again = IndexWriteLock::acquire(&graph_dir).unwrap();
        drop(again);
        assert!(IndexWriteLock::held_by_this_thread(&graph_dir));
        assert!(
            !another_thread_takes(&graph_dir),
            "another thread must not get a held lock"
        );
        drop(held);
        assert!(!IndexWriteLock::held_by_this_thread(&graph_dir));
        assert!(
            another_thread_takes(&graph_dir),
            "a released lock is free for another thread"
        );
    }

    /// D-i8-3: across the whole lifecycle of a guard (a fresh hold, a
    /// re-entrant hold beside it, the re-entrant hold dropped, the fresh
    /// hold dropped on the thread that took it), another thread never holds
    /// the lock at the same time, and the first thread, once it has
    /// released, is excluded by the second in turn rather than re-entering
    /// on a stale record of its own hold.
    #[test]
    #[cfg(target_os = "linux")]
    fn two_threads_exclude_each_other_across_the_guard_lifecycle() {
        use super::lock_waiters::wait_until_someone_waits;
        use std::sync::mpsc;
        let dir = tempfile::tempdir().unwrap();
        let graph_dir = dir.path().join(".sqry/graph");
        let inside = Arc::new(AtomicBool::new(false));
        let first = IndexWriteLock::acquire(&graph_dir).unwrap();
        // Index content, so the `try_acquire` probes below can take the
        // lock at all.
        fs::write(graph_dir.join("manifest.json"), b"{}").unwrap();
        inside.store(true, Ordering::SeqCst);
        let again = IndexWriteLock::acquire(&graph_dir).unwrap();
        let (to_main, from_other) = mpsc::channel::<&'static str>();
        let (to_other, from_main) = mpsc::channel::<()>();
        let other = {
            let (graph_dir, inside) = (graph_dir.clone(), Arc::clone(&inside));
            std::thread::spawn(move || {
                to_main.send("waiting").unwrap();
                let held = IndexWriteLock::acquire(&graph_dir).unwrap();
                assert!(
                    !inside.swap(true, Ordering::SeqCst),
                    "the second thread got the lock while the first still held it"
                );
                to_main.send("holding").unwrap();
                from_main.recv().unwrap();
                inside.store(false, Ordering::SeqCst);
                drop(held);
            })
        };
        assert_eq!(from_other.recv().unwrap(), "waiting");
        // The second thread is blocked on the lock, by the kernel's record.
        assert!(wait_until_someone_waits(&graph_dir, || other.is_finished()));
        drop(again);
        // A release would hand the lock to the waiting thread, which would
        // then panic (the first still holds) and finish; a waiter recorded
        // again while it has not finished means the lock was not released.
        // (A single read of `/proc/locks` can miss a line while other tests
        // change their locks, so only a positive reading is evidence.)
        assert!(
            wait_until_someone_waits(&graph_dir, || other.is_finished()),
            "dropping a re-entrant hold must not release the lock"
        );
        assert!(
            from_other.try_recv().is_err(),
            "the second thread must wait while the first holds"
        );
        inside.store(false, Ordering::SeqCst);
        drop(first);
        assert!(!IndexWriteLock::held_by_this_thread(&graph_dir));
        assert_eq!(from_other.recv().unwrap(), "holding");
        assert!(
            IndexWriteLock::try_acquire(&graph_dir).unwrap().is_none(),
            "the first thread must not re-enter a lock the second now holds"
        );
        to_other.send(()).unwrap();
        other.join().unwrap();
        let back = IndexWriteLock::try_acquire(&graph_dir).unwrap();
        assert!(back.is_some(), "a released lock is free again");
        assert!(!inside.load(Ordering::SeqCst));
    }

    /// D-i8-1: a reader that finds the manifest moved aside by a live
    /// transaction (another holder has the lock, the rollback names are
    /// present) waits for that transaction and reads what it committed; it
    /// never reports the root as unindexed.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_reader_waits_out_a_live_set_aside_window() {
        use crate::graph::unified::persistence::GraphStorage;
        use std::sync::mpsc;
        let dir = tempfile::tempdir().unwrap();
        let storage = GraphStorage::new(dir.path());
        fs::create_dir_all(storage.graph_dir()).unwrap();
        fs::write(storage.manifest_path(), b"{\"old\": true}").unwrap();
        let graph_dir = storage.graph_dir().to_path_buf();
        let manifest = storage.manifest_path().to_path_buf();
        let (to_main, from_writer) = mpsc::channel::<()>();
        let (release, released) = mpsc::channel::<()>();
        let writer = std::thread::spawn(move || {
            let _held = IndexWriteLock::acquire(&graph_dir).unwrap();
            let tag = format!("{ROLLBACK_MARK}{}-1-1", std::process::id());
            fs::rename(&manifest, graph_dir.join(format!(".manifest.json{tag}"))).unwrap();
            fs::write(
                graph_dir.join(format!("{MARKER_BEGUN}{tag}")),
                "snapshot_existed=1",
            )
            .unwrap();
            to_main.send(()).unwrap();
            released.recv().unwrap();
            // The commit point: the new manifest, then the set removed.
            fs::write(&manifest, b"{\"new\": true}").unwrap();
            for entry in fs::read_dir(&graph_dir).unwrap().flatten() {
                if entry.file_name().to_string_lossy().contains(ROLLBACK_MARK) {
                    fs::remove_file(entry.path()).unwrap();
                }
            }
        });
        from_writer.recv().unwrap();
        let reader = {
            let root = dir.path().to_path_buf();
            std::thread::spawn(move || GraphStorage::new(&root).exists())
        };
        // Release the transaction once the reader waits for it, or once it
        // returned without waiting (the defect), whichever comes first.
        super::lock_waiters::wait_until_someone_waits(storage.graph_dir(), || reader.is_finished());
        release.send(()).unwrap();
        assert!(
            reader.join().unwrap(),
            "a live transaction's set-aside window was read as no index"
        );
        writer.join().unwrap();
        assert_eq!(
            fs::read(storage.manifest_path()).unwrap(),
            b"{\"new\": true}",
            "the committed manifest stays; the reader recovered nothing over it"
        );
    }

    /// D-i8-6: a cancellable wait for a lock another thread holds gives up,
    /// holding nothing, once it is cancelled, and takes the lock once it is
    /// free. Each step is driven by the wait's own `contended` callback, not
    /// by time.
    #[test]
    fn a_cancellable_wait_gives_up_on_a_cancel_and_takes_a_free_lock() {
        use std::sync::mpsc;
        let dir = tempfile::tempdir().unwrap();
        let graph_dir = dir.path().join(".sqry/graph");
        let (held_tx, held_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let holder = {
            let graph_dir = graph_dir.clone();
            std::thread::spawn(move || {
                let _held = IndexWriteLock::acquire(&graph_dir).unwrap();
                held_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            })
        };
        held_rx.recv().unwrap();
        let cancelled = AtomicBool::new(false);
        let mut contended = 0;
        let gave_up = IndexWriteLock::acquire_unless_cancelled(
            &graph_dir,
            &|| cancelled.load(Ordering::SeqCst),
            &mut || {
                contended += 1;
                cancelled.store(true, Ordering::SeqCst);
            },
        )
        .unwrap();
        assert!(
            matches!(gave_up, LockWait::Cancelled),
            "a cancelled wait holds nothing: {gave_up:?}"
        );
        assert_eq!(contended, 1, "the contended callback runs once");
        assert!(!IndexWriteLock::held_by_this_thread(&graph_dir));
        release_tx.send(()).unwrap();
        holder.join().unwrap();
        let taken =
            IndexWriteLock::acquire_unless_cancelled(&graph_dir, &|| false, &mut || {}).unwrap();
        assert!(matches!(taken, LockWait::Held(_)), "a free lock is taken");
        assert!(IndexWriteLock::held_by_this_thread(&graph_dir));
    }

    /// D-i8-6 (audit N4): a wait whose index directory is removed while
    /// another holder keeps the lock ends promptly with
    /// [`LockWait::IndexRemoved`], holding nothing and creating nothing,
    /// without waiting for that holder. Before, the poll found no directory
    /// to lock and polled until cancelled.
    #[test]
    fn a_wait_ends_when_the_index_directory_is_removed() {
        use std::sync::mpsc;
        let dir = tempfile::tempdir().unwrap();
        let graph_dir = dir.path().join(".sqry/graph");
        let (held_tx, held_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let holder = {
            let graph_dir = graph_dir.clone();
            std::thread::spawn(move || {
                let _held = IndexWriteLock::acquire(&graph_dir).unwrap();
                held_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            })
        };
        held_rx.recv().unwrap();
        let (contended_tx, contended_rx) = mpsc::channel::<()>();
        let (done_tx, done_rx) = mpsc::channel::<String>();
        {
            let graph_dir = graph_dir.clone();
            std::thread::spawn(move || {
                let outcome =
                    IndexWriteLock::acquire_unless_cancelled(&graph_dir, &|| false, &mut || {
                        let _ = contended_tx.send(());
                    });
                let _ = done_tx.send(format!("{outcome:?}"));
            });
        }
        contended_rx.recv().unwrap();
        fs::remove_dir_all(dir.path().join(".sqry")).unwrap();
        let outcome = done_rx.recv_timeout(Duration::from_secs(3));
        release_tx.send(()).unwrap();
        holder.join().unwrap();
        assert_eq!(
            outcome.as_deref(),
            Ok("Ok(IndexRemoved)"),
            "the wait must end once the index is removed, while the holder still holds"
        );
        assert!(!graph_dir.exists(), "the wait created nothing again");
    }

    /// D-i8-6 (audit N5): the cancellable wait is not starved by blocking
    /// acquirers. Two threads alternate 50 ms holds with blocking acquires,
    /// as a busy CLI user's builds would; the cancellable wait must win a
    /// handoff within 5 s. Before, it polled with `try_lock` every 20 ms,
    /// lost every handoff to the woken blocking waiter, and never acquired.
    #[test]
    fn a_cancellable_wait_is_not_starved_by_blocking_waiters() {
        use std::sync::atomic::AtomicUsize;
        let dir = tempfile::tempdir().unwrap();
        let graph_dir = dir.path().join(".sqry/graph");
        fs::create_dir_all(&graph_dir).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let holds = Arc::new(AtomicUsize::new(0));
        let blockers: Vec<_> = (0..2)
            .map(|_| {
                let (graph_dir, stop, holds) =
                    (graph_dir.clone(), Arc::clone(&stop), Arc::clone(&holds));
                std::thread::spawn(move || {
                    while !stop.load(Ordering::SeqCst) {
                        let _held = IndexWriteLock::acquire(&graph_dir).unwrap();
                        holds.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(50));
                    }
                })
            })
            .collect();
        // Start once the blockers are trading the lock.
        while holds.load(Ordering::SeqCst) < 2 {
            std::thread::yield_now();
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let outcome = IndexWriteLock::acquire_unless_cancelled(
            &graph_dir,
            &|| std::time::Instant::now() > deadline,
            &mut || {},
        )
        .unwrap();
        let won = matches!(outcome, LockWait::Held(_));
        drop(outcome);
        stop.store(true, Ordering::SeqCst);
        for blocker in blockers {
            blocker.join().unwrap();
        }
        assert!(
            won,
            "the cancellable wait never won a handoff in 5 s ({} blocking holds)",
            holds.load(Ordering::SeqCst)
        );
    }

    /// Audit round 3, item 1: a hold taken on a lock file that was then
    /// removed (`rm -rf .sqry`) is stale. It must not make the holding
    /// thread's next acquire re-entrant while another thread holds the
    /// recreated lock file, and the stale guard must say it is no longer
    /// current. Before, the thread-local record named the directory only, so
    /// the next acquire returned at once and two threads held the index.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_stale_hold_is_not_re_entrant_once_the_lock_file_is_replaced() {
        use super::lock_waiters::wait_until_someone_waits;
        use std::sync::mpsc;
        let dir = tempfile::tempdir().unwrap();
        let graph_dir = dir.path().join(".sqry/graph");
        let (a_held_tx, a_held_rx) = mpsc::channel::<()>();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let (a_report_tx, a_report_rx) = mpsc::channel::<(bool, bool)>();
        let a_returned = Arc::new(AtomicBool::new(false));
        let a = {
            let (graph_dir, a_returned) = (graph_dir.clone(), Arc::clone(&a_returned));
            std::thread::spawn(move || {
                let first = IndexWriteLock::acquire(&graph_dir).unwrap();
                a_held_tx.send(()).unwrap();
                go_rx.recv().unwrap();
                let stale_reported_current = first.is_current();
                let again = IndexWriteLock::acquire(&graph_dir).unwrap();
                a_returned.store(true, Ordering::SeqCst);
                a_report_tx
                    .send((stale_reported_current, again.is_current()))
                    .unwrap();
                drop(again);
                drop(first);
            })
        };
        a_held_rx.recv().unwrap();
        fs::remove_dir_all(dir.path().join(".sqry")).unwrap();
        let (b_held_tx, b_held_rx) = mpsc::channel::<()>();
        let (b_release_tx, b_release_rx) = mpsc::channel::<()>();
        let b = {
            let graph_dir = graph_dir.clone();
            std::thread::spawn(move || {
                let _held = IndexWriteLock::acquire(&graph_dir).unwrap();
                b_held_tx.send(()).unwrap();
                b_release_rx.recv().unwrap();
            })
        };
        b_held_rx.recv().unwrap();
        go_tx.send(()).unwrap();
        let a_waited = wait_until_someone_waits(&graph_dir, || a_returned.load(Ordering::SeqCst));
        b_release_tx.send(()).unwrap();
        b.join().unwrap();
        let (stale_reported_current, again_current) = a_report_rx.recv().unwrap();
        a.join().unwrap();
        assert!(
            a_waited,
            "the thread with a stale hold re-entered while another thread held the current lock"
        );
        assert!(
            !stale_reported_current,
            "a hold on a removed lock file is not current"
        );
        assert!(
            again_current,
            "the hold taken after the wait is on the current lock file"
        );
    }

    /// Audit round 3, item 1: a blocking acquire that wins its `flock` on a
    /// lock file removed while it waited holds nothing anyone else
    /// respects. It must retry on the file the path names now, so a third
    /// acquirer waits for it. Before, it held the unlinked file and the
    /// third acquirer took the recreated one at once.
    #[test]
    #[cfg(target_os = "linux")]
    fn an_acquire_that_wins_an_unlinked_lock_file_retries_on_the_current_one() {
        use super::lock_waiters::wait_until_someone_waits;
        use std::sync::mpsc;
        let dir = tempfile::tempdir().unwrap();
        let graph_dir = dir.path().join(".sqry/graph");
        let (x_held_tx, x_held_rx) = mpsc::channel::<()>();
        let (x_release_tx, x_release_rx) = mpsc::channel::<()>();
        let x = {
            let graph_dir = graph_dir.clone();
            std::thread::spawn(move || {
                let _held = IndexWriteLock::acquire(&graph_dir).unwrap();
                x_held_tx.send(()).unwrap();
                x_release_rx.recv().unwrap();
            })
        };
        x_held_rx.recv().unwrap();
        let a_done = Arc::new(AtomicBool::new(false));
        let (a_held_tx, a_held_rx) = mpsc::channel::<()>();
        let (a_release_tx, a_release_rx) = mpsc::channel::<()>();
        let a = {
            let (graph_dir, a_done) = (graph_dir.clone(), Arc::clone(&a_done));
            std::thread::spawn(move || {
                let held = IndexWriteLock::acquire(&graph_dir).unwrap();
                a_done.store(true, Ordering::SeqCst);
                a_held_tx.send(()).unwrap();
                a_release_rx.recv().unwrap();
                drop(held);
            })
        };
        assert!(wait_until_someone_waits(&graph_dir, || a_done.load(Ordering::SeqCst)));
        fs::remove_dir_all(dir.path().join(".sqry")).unwrap();
        x_release_tx.send(()).unwrap();
        x.join().unwrap();
        a_held_rx.recv().unwrap();
        let c_done = Arc::new(AtomicBool::new(false));
        let c = {
            let (graph_dir, c_done) = (graph_dir.clone(), Arc::clone(&c_done));
            std::thread::spawn(move || {
                let _held = IndexWriteLock::acquire(&graph_dir).unwrap();
                c_done.store(true, Ordering::SeqCst);
            })
        };
        let c_waited = wait_until_someone_waits(&graph_dir, || c_done.load(Ordering::SeqCst));
        a_release_tx.send(()).unwrap();
        a.join().unwrap();
        c.join().unwrap();
        assert!(
            c_waited,
            "a third acquirer took the recreated lock file while the second held the removed one"
        );
    }

    /// Hold the persist lock of `graph_dir` on a thread until the returned
    /// sender is used (or dropped).
    fn hold_on_a_thread(
        graph_dir: &Path,
    ) -> (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
        use std::sync::mpsc;
        let (held_tx, held_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let graph_dir = graph_dir.to_path_buf();
        let holder = std::thread::spawn(move || {
            let _held = IndexWriteLock::acquire(&graph_dir).unwrap();
            held_tx.send(()).unwrap();
            let _ = release_rx.recv();
        });
        held_rx.recv().unwrap();
        (release_tx, holder)
    }

    /// Fourth audit, item 1: a helper that fails to start leaves no entry
    /// behind. The first wait reports the failure; a later wait for the
    /// same lock file starts its own helper and takes the lock once the
    /// holder releases. Before, the entry was registered before the spawn,
    /// stayed unfinished with no helper, and the later wait attached to it
    /// and never took the lock.
    #[test]
    fn a_helper_that_fails_to_start_leaves_no_dead_wait() {
        let dir = tempfile::tempdir().unwrap();
        let graph_dir = dir.path().join(".sqry/graph");
        let (release, holder) = hold_on_a_thread(&graph_dir);
        helper_seam::arm(
            &graph_dir.join(PERSIST_LOCK_FILE_NAME),
            helper_seam::Fault::SpawnFails,
        );
        let first = IndexWriteLock::acquire_unless_cancelled(&graph_dir, &|| false, &mut || {});
        assert!(first.is_err(), "the failed start is reported: {first:?}");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut release = Some(release);
        let second = IndexWriteLock::acquire_unless_cancelled(
            &graph_dir,
            &|| std::time::Instant::now() > deadline,
            &mut || {
                // Let the holder go once this wait is contended.
                drop(release.take());
            },
        )
        .unwrap();
        holder.join().unwrap();
        assert!(
            matches!(second, LockWait::Held(_)),
            "a later wait attached to the dead entry: {second:?}"
        );
    }

    /// Fourth audit, item 1: a helper that panics leaves no dead entry: its
    /// waiter starts another helper and takes the lock once the holder
    /// releases. Before, the entry stayed unfinished and the wait never
    /// took the lock.
    #[test]
    fn a_helper_that_panics_leaves_no_dead_wait() {
        let dir = tempfile::tempdir().unwrap();
        let graph_dir = dir.path().join(".sqry/graph");
        let (release, holder) = hold_on_a_thread(&graph_dir);
        helper_seam::arm(
            &graph_dir.join(PERSIST_LOCK_FILE_NAME),
            helper_seam::Fault::Panics,
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut release = Some(release);
        let outcome = IndexWriteLock::acquire_unless_cancelled(
            &graph_dir,
            &|| std::time::Instant::now() > deadline,
            &mut || drop(release.take()),
        )
        .unwrap();
        holder.join().unwrap();
        assert!(
            matches!(outcome, LockWait::Held(_)),
            "the wait stayed attached to a panicked helper: {outcome:?}"
        );
    }

    /// Fifth audit, item 1: over an index with no lock file (an older
    /// version wrote it), a removal that empties the directory while the
    /// lock file is being created leaves no lock for the persist to take.
    /// The seam removes the index content right after the creation; the
    /// wait answers `IndexRemoved` and the created lock file is left in
    /// place, unlocked (round nine: no lock or persist path removes a lock file).
    /// Before the fifth audit the wait took the lock on it.
    #[test]
    fn an_existing_index_wait_takes_no_lock_in_an_emptied_directory() {
        let dir = tempfile::tempdir().unwrap();
        let graph_dir = dir.path().join(".sqry/graph");
        fs::create_dir_all(&graph_dir).unwrap();
        fs::write(graph_dir.join("manifest.json"), b"{}").unwrap();
        let emptied = graph_dir.clone();
        lock_create_seam::arm(
            &graph_dir,
            Box::new(move || fs::remove_file(emptied.join("manifest.json")).unwrap()),
        );
        let outcome =
            IndexWriteLock::acquire_existing_unless_cancelled(&graph_dir, &|| false, &mut || {})
                .unwrap();
        let left: Vec<_> = fs::read_dir(&graph_dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name())
            .collect();
        assert!(
            matches!(outcome, LockWait::IndexRemoved),
            "a lock file created into a directory being emptied was taken: {outcome:?}"
        );
        assert_eq!(
            left,
            vec![std::ffi::OsString::from(PERSIST_LOCK_FILE_NAME)],
            "only the created lock file is left"
        );
        assert!(
            another_thread_takes_the_file(&graph_dir),
            "the left lock file is still locked"
        );
    }

    /// Whether another thread can `flock` the lock file of `graph_dir` now
    /// (it exists and nobody holds it).
    fn another_thread_takes_the_file(graph_dir: &Path) -> bool {
        let path = graph_dir.join(PERSIST_LOCK_FILE_NAME);
        std::thread::spawn(move || {
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .is_ok_and(|file| file.try_lock().is_ok())
        })
        .join()
        .unwrap()
    }

    /// Fifth audit, item 1, the same rule for `try_acquire` (which
    /// `recover_for_reader` uses): no lock file is created into a directory
    /// with no index content, and one created while the content is being
    /// removed is left in place, unlocked, with no lock taken. Before the
    /// fifth audit it created and locked one.
    #[test]
    fn try_acquire_takes_no_lock_in_an_emptied_directory() {
        let dir = tempfile::tempdir().unwrap();
        let graph_dir = dir.path().join(".sqry/graph");
        fs::create_dir_all(&graph_dir).unwrap();
        assert!(IndexWriteLock::try_acquire(&graph_dir).unwrap().is_none());
        assert!(
            !graph_dir.join(PERSIST_LOCK_FILE_NAME).exists(),
            "a lock file was created into a directory with no index"
        );
        fs::write(graph_dir.join("snapshot.sqry"), b"x").unwrap();
        let emptied = graph_dir.clone();
        lock_create_seam::arm(
            &graph_dir,
            Box::new(move || fs::remove_file(emptied.join("snapshot.sqry")).unwrap()),
        );
        assert!(IndexWriteLock::try_acquire(&graph_dir).unwrap().is_none());
        assert!(
            another_thread_takes_the_file(&graph_dir),
            "the created lock file is gone or still locked"
        );
    }

    /// Round ten: `holds_committed_index` answers whether a recovery run
    /// now leaves a manifest or a snapshot. Each expected value is stated
    /// from the transaction's meaning of the files, and checked against
    /// what `recover_interrupted_persist` then leaves.
    #[test]
    fn a_committed_index_is_what_recovery_would_leave() {
        let begun = |tag: &str, snapshot: u8, manifest: u8| {
            (
                format!("{MARKER_BEGUN}{ROLLBACK_MARK}{tag}"),
                format!("begun snapshot_existed={snapshot} manifest_existed={manifest}\n"),
            )
        };
        let file = |name: &str, body: &str| (name.to_string(), body.to_string());
        // A case: its name, the files it plants (name, content), the answer.
        type Case<'a> = (&'a str, Vec<(String, String)>, bool);
        let cases: Vec<Case<'_>> = vec![
            ("empty directory", vec![], false),
            (
                "lock file only",
                vec![file(PERSIST_LOCK_FILE_NAME, "")],
                false,
            ),
            (
                "manifest and snapshot",
                vec![file("manifest.json", "{}"), file("snapshot.sqry", "s")],
                true,
            ),
            ("snapshot only", vec![file("snapshot.sqry", "s")], true),
            ("manifest only", vec![file("manifest.json", "{}")], true),
            (
                "a killed first persist",
                vec![begun("1-1-1", 0, 0), file("snapshot.sqry", "partial")],
                false,
            ),
            (
                "a first persist killed before its snapshot",
                vec![begun("1-1-1", 0, 0)],
                false,
            ),
            (
                "a first persist killed after its manifest",
                vec![
                    begun("1-1-1", 0, 0),
                    file("snapshot.sqry", "new"),
                    file("manifest.json", "{}"),
                ],
                true,
            ),
            (
                "a killed update of a full index",
                vec![
                    begun("1-1-1", 1, 1),
                    file(&format!(".manifest.json{ROLLBACK_MARK}1-1-1"), "{}"),
                    file(&format!(".snapshot.sqry{ROLLBACK_MARK}1-1-1"), "old"),
                    file("snapshot.sqry", "partial"),
                ],
                true,
            ),
            (
                "a killed update of a snapshot-only index",
                vec![
                    begun("1-1-1", 1, 0),
                    file(&format!(".snapshot.sqry{ROLLBACK_MARK}1-1-1"), "old"),
                    file("snapshot.sqry", "partial"),
                ],
                true,
            ),
            (
                "an update killed before its snapshot was kept aside",
                vec![begun("1-1-1", 1, 0), file("snapshot.sqry", "old")],
                true,
            ),
            (
                "an older killed first persist and a newer killed update",
                vec![
                    begun("1-1-1", 0, 0),
                    begun("2-2-2", 1, 1),
                    file(&format!(".manifest.json{ROLLBACK_MARK}2-2-2"), "{}"),
                    file(&format!(".snapshot.sqry{ROLLBACK_MARK}2-2-2"), "old"),
                    file("snapshot.sqry", "partial"),
                ],
                true,
            ),
            (
                "a committed set left behind",
                vec![
                    file(&format!("{MARKER_COMMITTED}{ROLLBACK_MARK}1-1-1"), ""),
                    file("snapshot.sqry", "new"),
                    file("manifest.json", "{}"),
                ],
                true,
            ),
        ];
        assert_eq!(cases.len(), 13, "cases");
        for (case, files, expected) in cases {
            let dir = tempfile::tempdir().unwrap();
            let graph_dir = dir.path().join(".sqry/graph");
            fs::create_dir_all(&graph_dir).unwrap();
            for (name, body) in &files {
                fs::write(graph_dir.join(name), body).unwrap();
            }
            assert_eq!(holds_committed_index(&graph_dir), expected, "{case}");
            recover_interrupted_persist(&graph_dir).unwrap();
            let left = graph_dir.join("manifest.json").exists()
                || graph_dir.join("snapshot.sqry").exists();
            assert_eq!(left, expected, "{case}: what recovery left");
            assert!(!has_rollback_names(&graph_dir), "{case}: sets left");
        }
        let dir = tempfile::tempdir().unwrap();
        assert!(
            !holds_committed_index(&dir.path().join("missing")),
            "a missing directory"
        );
    }

    /// Round ten verification: a first persist that another writer begins
    /// and then rolls back between the reads of one `holds_committed_index`
    /// (its marker and snapshot created after the first look, the snapshot
    /// then the marker removed before the last) is no committed index. The
    /// directory never held one. The double listing of 20b38d394 answered
    /// true: both listings were empty and the snapshot was seen between
    /// them (the seam points were then after the first listing and after
    /// the presence check).
    #[test]
    fn a_first_persist_begun_and_rolled_back_during_the_read_is_no_index() {
        let dir = tempfile::tempdir().unwrap();
        let graph_dir = dir.path().join(".sqry/graph");
        fs::create_dir_all(&graph_dir).unwrap();
        let marker = graph_dir.join(format!("{MARKER_BEGUN}{ROLLBACK_MARK}1-1-1"));
        let snapshot = graph_dir.join("snapshot.sqry");
        {
            let (marker, snapshot) = (marker.clone(), snapshot.clone());
            committed_read_seam::arm(
                &graph_dir,
                committed_read_seam::Point::First,
                Box::new(move || {
                    fs::write(&marker, b"begun snapshot_existed=0 manifest_existed=0\n").unwrap();
                    fs::write(&snapshot, b"new").unwrap();
                }),
            );
        }
        committed_read_seam::arm(
            &graph_dir,
            committed_read_seam::Point::Second,
            Box::new(move || {
                fs::remove_file(&snapshot).unwrap();
                fs::remove_file(&marker).unwrap();
            }),
        );
        let answer = holds_committed_index(&graph_dir);
        let left: Vec<_> = fs::read_dir(&graph_dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name())
            .collect();
        assert!(left.is_empty(), "the writer left {left:?}");
        assert!(!answer, "a phantom committed index");
    }

    /// Round ten verification: a FIFO at the manifest, the snapshot or a
    /// begun marker's path does not block the committed-index read or
    /// recovery (each runs under the persist lock, in every fresh hold).
    /// It counts as present, as recovery's `exists()` counts it; an
    /// unreadable marker keeps the snapshot. A blocking open there waited
    /// for a writer for ever (the cases time out after 5 s).
    #[test]
    #[cfg(unix)]
    fn a_fifo_in_the_index_does_not_block_the_read_or_recovery() {
        let marker_name = format!("{MARKER_BEGUN}{ROLLBACK_MARK}1-1-1");
        let cases: [(&str, &str, Vec<&str>, bool); 4] = [
            ("snapshot", "snapshot.sqry", vec![], true),
            ("manifest", "manifest.json", vec![], true),
            ("marker", marker_name.as_str(), vec!["snapshot.sqry"], true),
            (
                "marker, recovery",
                marker_name.as_str(),
                vec!["snapshot.sqry"],
                true,
            ),
        ];
        for (case, fifo, files, expected) in cases {
            let dir = tempfile::tempdir().unwrap();
            let graph_dir = dir.path().join(".sqry/graph");
            fs::create_dir_all(&graph_dir).unwrap();
            for name in files {
                fs::write(graph_dir.join(name), b"s").unwrap();
            }
            let fifo = graph_dir.join(fifo);
            let made = std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .expect("run mkfifo");
            assert!(made.success(), "{case}: mkfifo failed");
            let (tx, rx) = std::sync::mpsc::channel();
            {
                let graph_dir = graph_dir.clone();
                let recover = case.ends_with("recovery");
                std::thread::spawn(move || {
                    let answer = if recover {
                        recover_interrupted_persist(&graph_dir).is_ok()
                    } else {
                        holds_committed_index(&graph_dir)
                    };
                    let _ = tx.send(answer);
                });
            }
            let answer = rx.recv_timeout(Duration::from_secs(5));
            if answer.is_err() {
                // Release the blocked reader before failing.
                let _ = OpenOptions::new().write(true).open(&fifo);
            }
            assert_eq!(answer, Ok(expected), "{case}");
        }
    }

    /// Round ten (Codex P2): a first persist that rolls back (snapshot,
    /// then marker) after the read's last look at the snapshot is no
    /// committed index. The marker is read during the listing, inside the
    /// interval the identity comparison covers. Read afterwards, its open
    /// failed, recovery's decision fell to keeping the snapshot seen
    /// earlier, and the answer was true.
    #[test]
    fn a_first_persist_rolled_back_after_the_last_look_is_no_index() {
        let dir = tempfile::tempdir().unwrap();
        let graph_dir = dir.path().join(".sqry/graph");
        fs::create_dir_all(&graph_dir).unwrap();
        begin_a_first_persist(&graph_dir, "1-1-1");
        let rolled_back = graph_dir.clone();
        committed_read_seam::arm(
            &graph_dir,
            committed_read_seam::Point::Third,
            Box::new(move || {
                fs::remove_file(rolled_back.join("snapshot.sqry")).unwrap();
                fs::remove_file(rolled_back.join(format!("{MARKER_BEGUN}{ROLLBACK_MARK}1-1-1")))
                    .unwrap();
            }),
        );
        assert!(
            !holds_committed_index(&graph_dir),
            "a phantom committed index"
        );
    }

    /// Write a first persist's begun marker, then its snapshot as the
    /// transaction writes it (a new file renamed into place).
    fn begin_a_first_persist(graph_dir: &Path, tag: &str) {
        fs::write(
            graph_dir.join(format!("{MARKER_BEGUN}{ROLLBACK_MARK}{tag}")),
            b"begun snapshot_existed=0 manifest_existed=0\n",
        )
        .unwrap();
        let partial = graph_dir.join(format!("snapshot.sqry.tmp-{tag}"));
        fs::write(&partial, tag.as_bytes()).unwrap();
        fs::rename(&partial, graph_dir.join("snapshot.sqry")).unwrap();
    }

    /// Round ten verification: a first persist that begins (marker, then
    /// snapshot) after the read's listing is seen as a change and read
    /// again, so the answer is no committed index. Without the identity
    /// comparison the read answered from the empty listing and the snapshot
    /// present after it: true.
    #[test]
    fn a_first_persist_begun_after_the_listing_is_no_index() {
        let dir = tempfile::tempdir().unwrap();
        let graph_dir = dir.path().join(".sqry/graph");
        fs::create_dir_all(&graph_dir).unwrap();
        let begun = graph_dir.clone();
        committed_read_seam::arm(
            &graph_dir,
            committed_read_seam::Point::Second,
            Box::new(move || begin_a_first_persist(&begun, "2-2-2")),
        );
        assert!(
            !holds_committed_index(&graph_dir),
            "a phantom committed index"
        );
    }

    /// Round ten verification (the verifier's probe): first persist A is
    /// in flight when the read starts and rolls back after its first look
    /// (snapshot, then marker); first persist B then begins after the
    /// listing. With device and inode taken by `stat` alone, B's snapshot
    /// took A's freed inode number (20 of 20 probe rounds on ext4) and read
    /// as A's unchanged snapshot: true. Neither ever committed. The
    /// mechanism is asserted directly, whatever the allocator does: when B
    /// begins, this process still holds A's unlinked snapshot open (Linux,
    /// `/proc/self/fd`), and B's snapshot has another inode.
    #[test]
    #[cfg(unix)]
    fn a_snapshot_replaced_during_the_read_is_seen_as_changed() {
        use std::os::unix::fs::MetadataExt;
        use std::sync::Mutex;
        let dir = tempfile::tempdir().unwrap();
        let graph_dir = dir.path().join(".sqry/graph");
        fs::create_dir_all(&graph_dir).unwrap();
        begin_a_first_persist(&graph_dir, "1-1-1");
        let snapshot = graph_dir.join("snapshot.sqry");
        let ino_a = fs::metadata(&snapshot).unwrap().ino();
        let rolled_back = graph_dir.clone();
        committed_read_seam::arm(
            &graph_dir,
            committed_read_seam::Point::First,
            Box::new(move || {
                fs::remove_file(rolled_back.join("snapshot.sqry")).unwrap();
                fs::remove_file(rolled_back.join(format!("{MARKER_BEGUN}{ROLLBACK_MARK}1-1-1")))
                    .unwrap();
            }),
        );
        // Whether A's unlinked snapshot was held open when B began, and B's
        // snapshot inode.
        let seen: Arc<Mutex<Option<(bool, u64)>>> = Arc::default();
        {
            let seen = Arc::clone(&seen);
            let begun = graph_dir.clone();
            let deleted = format!("{} (deleted)", snapshot.display());
            committed_read_seam::arm(
                &graph_dir,
                committed_read_seam::Point::Second,
                Box::new(move || {
                    let held = held_open(&deleted);
                    begin_a_first_persist(&begun, "2-2-2");
                    let ino_b = fs::metadata(begun.join("snapshot.sqry")).unwrap().ino();
                    *seen.lock().unwrap() = Some((held, ino_b));
                }),
            );
        }
        let answer = holds_committed_index(&graph_dir);
        let (held, ino_b) = seen.lock().unwrap().expect("the seam ran");
        assert!(held, "A's unlinked snapshot was not held open when B began");
        assert_ne!(ino_b, ino_a, "B's snapshot took A's inode");
        assert!(!answer, "a phantom committed index");
    }

    /// Whether this process holds a file descriptor whose link in
    /// `/proc/self/fd` reads `target` (Linux); elsewhere, `true` (the
    /// precondition cannot be read there, and the inode assertion stands).
    #[cfg(unix)]
    fn held_open(target: &str) -> bool {
        if !cfg!(target_os = "linux") {
            return true;
        }
        let entries = fs::read_dir("/proc/self/fd").expect("read /proc/self/fd");
        entries.flatten().any(|entry| {
            fs::read_link(entry.path()).is_ok_and(|link| link.to_string_lossy() == target)
        })
    }

    /// Round ten: a wait reports an index it saw when it opened the lock
    /// file, and one committed while it waited through the commit recorded
    /// on the lock file, even when the user removed that index content
    /// (lock file kept) before the hold arrived; a commit not recorded
    /// there is not seen.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_wait_reports_an_index_it_saw() {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum Case {
            AtOpen,
            CommittedRecorded,
            CommittedUnrecorded,
        }
        for case in [
            Case::AtOpen,
            Case::CommittedRecorded,
            Case::CommittedUnrecorded,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let graph_dir = dir.path().join(".sqry/graph");
            let held = IndexWriteLock::acquire(&graph_dir).unwrap();
            assert!(!held.saw_committed_index(), "{case:?}: no index yet");
            if case == Case::AtOpen {
                fs::write(graph_dir.join("manifest.json"), b"{}").unwrap();
            }
            let waiter = {
                let graph_dir = graph_dir.clone();
                std::thread::spawn(move || {
                    match IndexWriteLock::acquire_unless_cancelled(
                        &graph_dir,
                        &|| false,
                        &mut || {},
                    )
                    .unwrap()
                    {
                        LockWait::Held(held) => held.saw_committed_index(),
                        other => panic!("the wait answered {other:?}"),
                    }
                })
            };
            assert!(
                lock_waiters::wait_until_someone_waits(&graph_dir, || waiter.is_finished()),
                "{case:?}: the wait was not contended"
            );
            if case != Case::AtOpen {
                fs::write(graph_dir.join("manifest.json"), b"{}").unwrap();
            }
            if case == Case::CommittedRecorded {
                held.record_commit().unwrap();
            }
            // The user removes the index content and keeps the lock file.
            fs::remove_file(graph_dir.join("manifest.json")).unwrap();
            drop(held);
            assert_eq!(
                waiter.join().unwrap(),
                case != Case::CommittedUnrecorded,
                "{case:?}"
            );
        }
    }

    /// Round ten: a fresh hold and a re-entrant guard report the committed
    /// index in place when they were made.
    #[test]
    fn a_hold_reports_the_committed_index_it_found() {
        let dir = tempfile::tempdir().unwrap();
        let graph_dir = dir.path().join(".sqry/graph");
        fs::create_dir_all(&graph_dir).unwrap();
        fs::write(
            graph_dir.join(format!("{MARKER_BEGUN}{ROLLBACK_MARK}1-1-1")),
            b"begun snapshot_existed=0 manifest_existed=0\n",
        )
        .unwrap();
        fs::write(graph_dir.join("snapshot.sqry"), b"partial").unwrap();
        let held = IndexWriteLock::acquire(&graph_dir).unwrap();
        assert!(!held.saw_committed_index(), "a first persist rolled back");
        assert!(!graph_dir.join("snapshot.sqry").exists());
        fs::write(graph_dir.join("snapshot.sqry"), b"s").unwrap();
        let again = IndexWriteLock::reenter(&graph_dir).expect("re-entry");
        assert!(again.saw_committed_index(), "a snapshot in place");
        drop((again, held));
    }

    /// How one existing-index attempt answered, for the content tests:
    /// the wait's outcome, or `try_take`'s.
    fn existing_index_attempt(graph_dir: &Path, via_wait: bool) -> &'static str {
        if via_wait {
            match IndexWriteLock::acquire_existing_unless_cancelled(
                graph_dir,
                &|| false,
                &mut || {},
            )
            .unwrap()
            {
                LockWait::Held(_) => "Held",
                LockWait::Cancelled => "Cancelled",
                LockWait::IndexRemoved => "IndexRemoved",
            }
        } else {
            match IndexWriteLock::try_take(graph_dir).unwrap() {
                TryTake::Taken(_) => "Taken",
                TryTake::Busy => "Busy",
                TryTake::NoIndex => "NoIndex",
            }
        }
    }

    /// The lock file `other` is still the one at `lock_path` and nobody
    /// holds a lock on it: the attempt neither removed nor kept it.
    fn assert_left_untouched(case: &str, other: &File, lock_path: &Path, graph_dir: &Path) {
        assert!(
            LockFileIdentity::of(other).matches(lock_path),
            "{case}: the lock file another opener owns was removed"
        );
        assert!(
            !IndexWriteLock::held_any_by_this_thread(graph_dir),
            "{case}: a hold was kept"
        );
        assert!(
            other.try_lock().is_ok(),
            "{case}: the lock file another opener owns is still locked"
        );
        other.unlock().unwrap();
    }

    /// Round-nine verification, owner decision "re-check content on both
    /// paths": a lock file another opener creates between the first open
    /// finding none and this call's own creation is that opener's. When
    /// the index content is then found gone, the existing-index wait
    /// answers `IndexRemoved` and `try_take` `NoIndex`, and the file is
    /// left at the path, unlocked and never removed. The seam, run in that
    /// window, creates the file as another opener (keeping it open) and
    /// removes the manifest. With a plain create the file was removed as
    /// this call's own; with only the exclusive create it was taken (`Held`,
    /// `Taken`) over an index with no content.
    #[test]
    #[cfg(unix)]
    fn a_lock_file_another_opener_created_in_the_window_is_left_and_not_taken() {
        for via_wait in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let graph_dir = dir.path().join(".sqry/graph");
            fs::create_dir_all(&graph_dir).unwrap();
            fs::write(graph_dir.join("manifest.json"), b"{}").unwrap();
            let lock_path = graph_dir.join(PERSIST_LOCK_FILE_NAME);
            let (other_tx, other_rx) = std::sync::mpsc::channel::<File>();
            {
                let (lock_path, graph_dir) = (lock_path.clone(), graph_dir.clone());
                lock_create_seam::arm_before(
                    &graph_dir.clone(),
                    Box::new(move || {
                        let other = OpenOptions::new()
                            .read(true)
                            .write(true)
                            .create_new(true)
                            .open(&lock_path)
                            .unwrap();
                        fs::remove_file(graph_dir.join("manifest.json")).unwrap();
                        other_tx.send(other).unwrap();
                    }),
                );
            }
            let case = format!("via_wait={via_wait}");
            let answer = existing_index_attempt(&graph_dir, via_wait);
            let other = other_rx
                .try_recv()
                .unwrap_or_else(|_| panic!("{case}: the seam never ran"));
            assert_left_untouched(&case, &other, &lock_path, &graph_dir);
            let expected = if via_wait { "IndexRemoved" } else { "NoIndex" };
            assert_eq!(
                answer, expected,
                "{case}: an index with no content was locked"
            );
        }
    }

    /// Round-nine verification: a lock file another opener creates in the
    /// window, with the index content kept, is that opener's and is taken
    /// as one present at entry. The existing-index wait answers `Held` and
    /// `try_take` `Taken`, on that very file (identity equal), and the file
    /// stays at the path.
    #[test]
    #[cfg(unix)]
    fn a_lock_file_another_opener_created_with_content_kept_is_taken() {
        for via_wait in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let graph_dir = dir.path().join(".sqry/graph");
            fs::create_dir_all(&graph_dir).unwrap();
            fs::write(graph_dir.join("manifest.json"), b"{}").unwrap();
            let lock_path = graph_dir.join(PERSIST_LOCK_FILE_NAME);
            let (other_tx, other_rx) = std::sync::mpsc::channel::<File>();
            {
                let lock_path = lock_path.clone();
                lock_create_seam::arm_before(
                    &graph_dir,
                    Box::new(move || {
                        let other = OpenOptions::new()
                            .read(true)
                            .write(true)
                            .create_new(true)
                            .open(&lock_path)
                            .unwrap();
                        other_tx.send(other).unwrap();
                    }),
                );
            }
            let case = format!("via_wait={via_wait}");
            let taken = if via_wait {
                match IndexWriteLock::acquire_existing_unless_cancelled(
                    &graph_dir,
                    &|| false,
                    &mut || {},
                )
                .unwrap()
                {
                    LockWait::Held(held) => Ok(held),
                    other => Err(format!("{other:?}")),
                }
            } else {
                match IndexWriteLock::try_take(&graph_dir).unwrap() {
                    TryTake::Taken(held) => Ok(held),
                    TryTake::Busy => Err("Busy".to_string()),
                    TryTake::NoIndex => Err("NoIndex".to_string()),
                }
            };
            let other = other_rx
                .try_recv()
                .unwrap_or_else(|_| panic!("{case}: the seam never ran"));
            let held = taken.unwrap_or_else(|answer| {
                panic!("{case}: the other opener's file over an index was not taken: {answer}")
            });
            assert_eq!(
                held.identity,
                LockFileIdentity::of(&other),
                "{case}: the lock was taken on another file than the other opener's"
            );
            assert!(
                LockFileIdentity::of(&other).matches(&lock_path),
                "{case}: the other opener's lock file was removed"
            );
        }
    }

    /// Round-nine verification: a lock file another opener creates in the
    /// window and removes again before it is opened (the exclusive create
    /// finds it, the open then does not) is no index. The existing-index
    /// wait answers `IndexRemoved` and `try_take` `NoIndex`, as an error
    /// would not, and nothing is created: the directory keeps only what it
    /// had.
    #[test]
    fn a_lock_file_gone_again_after_another_opener_created_it_is_no_index() {
        for via_wait in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let graph_dir = dir.path().join(".sqry/graph");
            fs::create_dir_all(&graph_dir).unwrap();
            fs::write(graph_dir.join("manifest.json"), b"{}").unwrap();
            let lock_path = graph_dir.join(PERSIST_LOCK_FILE_NAME);
            {
                let lock_path = lock_path.clone();
                lock_create_seam::arm_before(
                    &graph_dir,
                    Box::new(move || {
                        OpenOptions::new()
                            .read(true)
                            .write(true)
                            .create_new(true)
                            .open(&lock_path)
                            .unwrap();
                    }),
                );
            }
            {
                let lock_path = lock_path.clone();
                lock_create_seam::arm_after_already_exists(
                    &graph_dir,
                    Box::new(move || fs::remove_file(&lock_path).unwrap()),
                );
            }
            let case = format!("via_wait={via_wait}");
            let answer = if via_wait {
                IndexWriteLock::acquire_existing_unless_cancelled(&graph_dir, &|| false, &mut || {})
                    .map(|outcome| format!("{outcome:?}"))
            } else {
                IndexWriteLock::try_take(&graph_dir).map(|taken| {
                    match taken {
                        TryTake::Taken(_) => "Taken",
                        TryTake::Busy => "Busy",
                        TryTake::NoIndex => "NoIndex",
                    }
                    .to_string()
                })
            }
            .unwrap_or_else(|e| format!("Err({e:#})"));
            let expected = if via_wait { "IndexRemoved" } else { "NoIndex" };
            assert_eq!(answer, expected, "{case}");
            let left: Vec<_> = fs::read_dir(&graph_dir)
                .unwrap()
                .flatten()
                .map(|entry| entry.file_name())
                .collect();
            assert_eq!(
                left,
                vec![std::ffi::OsString::from("manifest.json")],
                "{case}: something was created"
            );
        }
    }

    /// Round-nine verification, owner decision "re-check content on both
    /// paths": a lock file present at entry in a directory with no index
    /// content (no manifest, no snapshot, no rollback set) is no index.
    /// The existing-index wait answers `IndexRemoved` and `try_take`
    /// `NoIndex`, and the file is left at the path, unlocked and never
    /// removed (it is not this call's). Before, both opened it as it was
    /// and took the lock on it.
    #[test]
    fn a_lock_file_present_without_index_content_is_left_and_not_taken() {
        for via_wait in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let graph_dir = dir.path().join(".sqry/graph");
            fs::create_dir_all(&graph_dir).unwrap();
            let lock_path = graph_dir.join(PERSIST_LOCK_FILE_NAME);
            let other = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&lock_path)
                .unwrap();
            let case = format!("via_wait={via_wait}");
            let answer = existing_index_attempt(&graph_dir, via_wait);
            assert_left_untouched(&case, &other, &lock_path, &graph_dir);
            let expected = if via_wait { "IndexRemoved" } else { "NoIndex" };
            assert_eq!(
                answer, expected,
                "{case}: an index with no content was locked"
            );
        }
    }

    /// Verification after the fifth audit: a reader that finds a rollback
    /// set and no manifest, while the index directory is being removed,
    /// creates nothing. The seam removes `.sqry` right after the lock file
    /// is created; the reader must then end with no `.sqry` at all. Before,
    /// `recover_for_reader` read `try_acquire`'s "no lock" as a live
    /// transaction and called the creating `acquire`, which recreated
    /// `.sqry/graph/.persist.lock`.
    #[test]
    fn a_reader_recovery_creates_nothing_when_the_index_is_removed() {
        use crate::graph::unified::persistence::GraphStorage;
        let dir = tempfile::tempdir().unwrap();
        let storage = GraphStorage::new(dir.path());
        let graph_dir = storage.graph_dir().to_path_buf();
        fs::create_dir_all(&graph_dir).unwrap();
        // A rollback set an interrupted persist left, with no manifest.
        fs::write(
            graph_dir.join(format!(".manifest.json{ROLLBACK_MARK}1-1-1")),
            b"{}",
        )
        .unwrap();
        let sqry_dir = dir.path().join(".sqry");
        let removed = sqry_dir.clone();
        lock_create_seam::arm(
            &graph_dir,
            Box::new(move || fs::remove_dir_all(&removed).unwrap()),
        );
        let exists = storage.exists();
        let entries: Vec<_> = fs::read_dir(&graph_dir)
            .map(|entries| entries.flatten().map(|entry| entry.file_name()).collect())
            .unwrap_or_default();
        assert!(!exists, "no index is reported");
        assert!(
            !sqry_dir.exists(),
            "the reader recreated the removed index directory: {entries:?}"
        );
    }

    /// The `Busy` arm of `recover_for_reader`: a reader that finds a live
    /// transaction's window (manifest missing, rollback names present,
    /// another holder on the lock) waits for it, and if the index directory
    /// is removed while it waits it returns and creates nothing, before or
    /// after the holder releases. With that arm calling the creating
    /// `acquire`, the reader won the removed lock file on release, retried
    /// on the path and recreated `.sqry/graph/.persist.lock`.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_waiting_reader_creates_nothing_when_the_index_is_removed() {
        use super::lock_waiters::wait_until_someone_waits;
        use crate::graph::unified::persistence::GraphStorage;
        use std::sync::mpsc;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let graph_dir = GraphStorage::new(&root).graph_dir().to_path_buf();
        let (held_tx, held_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let holder = {
            let graph_dir = graph_dir.clone();
            std::thread::spawn(move || {
                let _held = IndexWriteLock::acquire(&graph_dir).unwrap();
                // The holder's live window: the manifest set aside.
                fs::write(
                    graph_dir.join(format!(".manifest.json{ROLLBACK_MARK}1-1-1")),
                    b"{}",
                )
                .unwrap();
                held_tx.send(()).unwrap();
                let _ = release_rx.recv();
            })
        };
        held_rx.recv().unwrap();
        let reader = {
            let root = root.clone();
            std::thread::spawn(move || GraphStorage::new(&root).exists())
        };
        assert!(
            wait_until_someone_waits(&graph_dir, || reader.is_finished()),
            "the reader did not wait for the live transaction"
        );
        fs::remove_dir_all(root.join(".sqry")).unwrap();
        release_tx.send(()).unwrap();
        holder.join().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !reader.is_finished() {
            assert!(
                std::time::Instant::now() < deadline,
                "the reader kept waiting after the index was removed"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let exists = reader.join().unwrap();
        let entries: Vec<_> = fs::read_dir(&graph_dir)
            .map(|entries| entries.flatten().map(|entry| entry.file_name()).collect())
            .unwrap_or_default();
        assert!(!exists, "no index is reported");
        assert!(
            !root.join(".sqry").exists(),
            "the waiting reader recreated the removed index directory: {entries:?}"
        );
    }

    /// A hold taken under one spelling of the graph directory is this
    /// thread's hold under another (a symlink here; a relative path is the
    /// same case): re-entering through the other spelling succeeds, and a
    /// reader on this thread that finds the live window under the other
    /// spelling returns instead of waiting for its own lock forever.
    /// Before, the hold record matched by path text, so the reader waited
    /// on itself.
    #[test]
    #[cfg(unix)]
    fn a_hold_is_found_under_another_spelling_of_the_directory() {
        use crate::graph::unified::persistence::GraphStorage;
        use std::sync::mpsc;
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        fs::create_dir_all(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let (done_tx, done_rx) = mpsc::channel::<(bool, bool)>();
        // On a thread of its own, so a self-wait fails the test instead of
        // hanging it.
        std::thread::spawn(move || {
            let real_graph = GraphStorage::new(&real).graph_dir().to_path_buf();
            let link_graph = GraphStorage::new(&link).graph_dir().to_path_buf();
            let held = IndexWriteLock::acquire(&real_graph).unwrap();
            fs::write(
                real_graph.join(format!(".manifest.json{ROLLBACK_MARK}1-1-1")),
                b"{}",
            )
            .unwrap();
            let reentered = IndexWriteLock::reenter(&link_graph).is_some();
            // The reader returns (its own hold is the transaction).
            let _ = GraphStorage::new(&link).exists();
            drop(held);
            let _ = done_tx.send((reentered, true));
        });
        let outcome = done_rx.recv_timeout(Duration::from_secs(10));
        assert_eq!(
            outcome,
            Ok((true, true)),
            "a hold under one spelling was not this thread's hold under another \
             (Err(Timeout): the reader waited on its own lock)"
        );
    }

    /// Whether another thread could take the lock of `graph_dir` now. The
    /// directory must hold index content: a probe that finds no index
    /// (`NoIndex`) panics, so "not taken" always means another holder has
    /// the lock.
    fn another_thread_takes(graph_dir: &Path) -> bool {
        let graph_dir = graph_dir.to_path_buf();
        std::thread::spawn(
            move || match IndexWriteLock::try_take(&graph_dir).unwrap() {
                TryTake::Taken(_) => true,
                TryTake::Busy => false,
                TryTake::NoIndex => panic!("the probe found no index in {}", graph_dir.display()),
            },
        )
        .join()
        .unwrap()
    }

    /// Codex round-eight review, item 1: the lock stays held, and this
    /// thread keeps its hold record, until the LAST guard for it drops, in
    /// either drop order and for a re-entrant guard from `acquire` or from
    /// `reenter`. Another thread is excluded until then and takes the lock
    /// right after. Before, dropping the original guard unlocked the file
    /// and removed the record while a re-entrant guard was alive.
    #[test]
    fn the_lock_is_held_until_the_last_guard_drops() {
        for (via_reenter, original_first) in
            [(false, true), (false, false), (true, true), (true, false)]
        {
            let dir = tempfile::tempdir().unwrap();
            let graph_dir = dir.path().join(".sqry/graph");
            let original = IndexWriteLock::acquire(&graph_dir).unwrap();
            // Index content, so the probes below can take the lock at all.
            fs::write(graph_dir.join("manifest.json"), b"{}").unwrap();
            let reentrant = if via_reenter {
                IndexWriteLock::reenter(&graph_dir).expect("a current hold to re-enter")
            } else {
                IndexWriteLock::acquire(&graph_dir).unwrap()
            };
            let case = format!("via_reenter={via_reenter} original_first={original_first}");
            let survivor = if original_first {
                drop(original);
                reentrant
            } else {
                drop(reentrant);
                original
            };
            assert!(
                IndexWriteLock::held_by_this_thread(&graph_dir),
                "{case}: the thread lost its hold record with a guard alive"
            );
            assert!(survivor.is_current(), "{case}");
            assert!(
                !another_thread_takes(&graph_dir),
                "{case}: another thread took the lock while a guard was alive"
            );
            drop(survivor);
            assert!(!IndexWriteLock::held_by_this_thread(&graph_dir), "{case}");
            assert!(
                another_thread_takes(&graph_dir),
                "{case}: the lock was not released when the last guard dropped"
            );
        }
    }

    /// Claude round-eight observation: on a filesystem whose lock-file
    /// identity never matches after `flock` (unstable inode numbers), the
    /// identity retries are bounded. `acquire` fails with the typed
    /// `UnstableLockFile` naming the path; `try_acquire` takes nothing.
    /// Each runs on a thread with a 10 s bound, so an unbounded retry
    /// fails the test instead of hanging it. Before, both spun for ever.
    #[test]
    fn the_identity_retries_are_bounded() {
        use std::sync::mpsc;
        let dir = tempfile::tempdir().unwrap();
        let graph_dir = dir.path().join(".sqry/graph");
        fs::create_dir_all(&graph_dir).unwrap();
        fs::write(graph_dir.join("manifest.json"), b"{}").unwrap();
        identity_seam::arm(&graph_dir);
        let (tx, rx) = mpsc::channel::<(String, bool)>();
        {
            let graph_dir = graph_dir.clone();
            std::thread::spawn(move || {
                let acquired = match IndexWriteLock::acquire(&graph_dir) {
                    Ok(_) => "Ok".to_string(),
                    Err(err) => {
                        let typed = err.chain().any(|cause| cause.is::<UnstableLockFile>());
                        format!("Err(typed={typed}): {err:#}")
                    }
                };
                let tried = IndexWriteLock::try_acquire(&graph_dir).unwrap().is_some();
                let _ = tx.send((acquired, tried));
            });
        }
        let (acquired, tried) = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the identity retries never ended");
        assert!(
            acquired.starts_with("Err(typed=true)") && acquired.contains("unstable"),
            "{acquired}"
        );
        assert!(acquired.contains(".persist.lock"), "{acquired}");
        assert!(
            !tried,
            "try_acquire took a lock whose identity never matched"
        );
    }

    /// Codex round-nine review, item 1: both cancellable waits check the
    /// cancellation on the re-entrant path too. With a cancel closure that
    /// always answers true, each answers `Cancelled` (and calls the closure)
    /// whether or not this thread already holds the lock, and a `Cancelled`
    /// re-entrant attempt leaves the original hold as it was: the record
    /// kept, the lock still excluding another thread. Before, a thread that
    /// held the lock got `Held` with the closure never called.
    #[test]
    fn cancellation_is_checked_with_and_without_a_prior_hold() {
        for existing in [false, true] {
            for prior_hold in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let graph_dir = dir.path().join(".sqry/graph");
                fs::create_dir_all(&graph_dir).unwrap();
                fs::write(graph_dir.join("manifest.json"), b"{}").unwrap();
                let original = prior_hold.then(|| IndexWriteLock::acquire(&graph_dir).unwrap());
                let calls = std::cell::Cell::new(0usize);
                let cancelled = || {
                    calls.set(calls.get() + 1);
                    true
                };
                let outcome = if existing {
                    IndexWriteLock::acquire_existing_unless_cancelled(
                        &graph_dir,
                        &cancelled,
                        &mut || {},
                    )
                } else {
                    IndexWriteLock::acquire_unless_cancelled(&graph_dir, &cancelled, &mut || {})
                }
                .unwrap();
                let case = format!("existing={existing} prior_hold={prior_hold}");
                assert!(
                    matches!(outcome, LockWait::Cancelled),
                    "{case}: {outcome:?} after {} cancel checks",
                    calls.get()
                );
                assert!(
                    calls.get() >= 1,
                    "{case}: the cancellation was never checked"
                );
                drop(outcome);
                if let Some(original) = original {
                    assert!(
                        IndexWriteLock::held_by_this_thread(&graph_dir),
                        "{case}: the cancelled attempt dropped the original hold's record"
                    );
                    assert!(original.is_current(), "{case}");
                    assert!(
                        !another_thread_takes(&graph_dir),
                        "{case}: the cancelled attempt released the original hold"
                    );
                    drop(original);
                }
                assert!(
                    another_thread_takes(&graph_dir),
                    "{case}: the lock was not free at the end"
                );
            }
        }
    }

    /// The re-entrant `IndexRemoved` outcome: a thread that holds the lock,
    /// and whose lock file (or whole `.sqry`) is removed by the time the
    /// cancellable wait re-enters (here from inside the cancel closure,
    /// which then answers false), gets `IndexRemoved` from both waits,
    /// with nothing recreated. Taking the lock afresh there (`acquire`
    /// instead of `reenter`) would recreate the lock file, and the
    /// directory with it.
    #[test]
    fn a_re_entrant_wait_over_a_removed_lock_file_creates_nothing() {
        for existing in [false, true] {
            for whole_dir in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let sqry_dir = dir.path().join(".sqry");
                let graph_dir = sqry_dir.join("graph");
                fs::create_dir_all(&graph_dir).unwrap();
                fs::write(graph_dir.join("manifest.json"), b"{}").unwrap();
                let held = IndexWriteLock::acquire(&graph_dir).unwrap();
                let lock_file = graph_dir.join(PERSIST_LOCK_FILE_NAME);
                let remove = || {
                    if whole_dir {
                        let _ = fs::remove_dir_all(&sqry_dir);
                    } else {
                        let _ = fs::remove_file(&lock_file);
                    }
                    false
                };
                let outcome = if existing {
                    IndexWriteLock::acquire_existing_unless_cancelled(
                        &graph_dir,
                        &remove,
                        &mut || {},
                    )
                } else {
                    IndexWriteLock::acquire_unless_cancelled(&graph_dir, &remove, &mut || {})
                }
                .unwrap();
                let case = format!("existing={existing} whole_dir={whole_dir}");
                assert!(
                    matches!(outcome, LockWait::IndexRemoved),
                    "{case}: {outcome:?}"
                );
                drop(outcome);
                assert!(!lock_file.exists(), "{case}: the lock file was recreated");
                if whole_dir {
                    assert!(!sqry_dir.exists(), "{case}: the directory was recreated");
                }
                drop(held);
            }
        }
    }

    #[test]
    fn the_gate_waits_for_a_persist_in_flight_and_refuses_new_ones() {
        let gate = Arc::new(PersistGate::new());
        let finished = Arc::new(AtomicBool::new(false));
        let entered = Arc::new(std::sync::Barrier::new(2));
        let worker = {
            let (gate, finished, entered) = (
                Arc::clone(&gate),
                Arc::clone(&finished),
                Arc::clone(&entered),
            );
            std::thread::spawn(move || {
                let _ticket = gate.enter().unwrap();
                entered.wait();
                std::thread::sleep(Duration::from_millis(300));
                finished.store(true, Ordering::Release);
            })
        };
        entered.wait();
        gate.close_and_wait();
        assert!(
            finished.load(Ordering::Acquire),
            "close_and_wait returned before the persist in flight finished"
        );
        assert!(gate.enter().is_err(), "a closed gate refuses a new persist");
        worker.join().unwrap();
    }
}
