//! The runtime environment-lock check: every environment access a lib unit
//! test of this crate makes through the C library's `getenv` family, traced
//! as it happens (Linux with glibc, test builds only).
//!
//! The static gate (`tests/env_lock_discipline.rs`) reads the parse, so it
//! cannot see what a method call or code outside the crate does. This check
//! runs the code instead. Two parts:
//!
//! - **The interposer.** This module defines `getenv`, `secure_getenv`,
//!   `setenv`, `unsetenv`, `putenv` and `clearenv` with C linkage, so the lib
//!   test binary's own calls (the standard library's `std::env` among them,
//!   and every crate linked in statically) bind to these definitions instead
//!   of the C library's. Each one forwards to the C library's function, found
//!   with `dlsym(RTLD_NEXT, ..)` (`real`). A lookup that finds nothing aborts
//!   the process with a message on standard error rather than answer every
//!   access wrongly in silence: that is what musl's `dlsym(RTLD_NEXT, ..)`
//!   does for these, which is why the module is compiled for glibc only
//!   (`target_env = "gnu"`). When the variable `SQRY_DAEMON_ENV_TRACE` names a
//!   file at process start (read once, through the C library's `getenv`, by a
//!   constructor in `.init_array`), each access is appended to it as one line:
//!   the operation (`get`, `secure_get`, `set`, `unset`, `put` or `clear`,
//!   one per function), the process id, the thread id, the holder of
//!   [`crate::TEST_ENV_LOCK`] at that moment ([`crate::test_env_lock`], a
//!   thread id, or 0 when the lock is free), and the variable's name. Every
//!   `write` is checked: a short or failed one is counted, and at exit a
//!   destructor in `.fini_array` appends an end line with the lines written
//!   whole and the writes that failed (`encode_end`). Without the variable
//!   the interposer only forwards.
//! - **The driver**, [`every_environment_access_of_a_lib_test_holds_the_crate_lock`].
//!   It lists this binary's tests (`--list`), and runs every one that `cargo
//!   test` runs (the ignored ones and itself excepted) in a process of its
//!   own, with `--exact <name> --test-threads=1`, the trace on and a time
//!   limit (`TEST_TIMEOUT`), from a snapshot of the environment taken under
//!   the crate lock (less `TERM`, so the harness's terminal probe reads one
//!   variable whatever the environment holds). In that process the main
//!   thread is the test harness (libtest runs the test on a thread of its own
//!   and waits for it), so an access on the main thread is the harness's, and
//!   every other access is the test's, including those of a process the test
//!   starts that runs this same binary (it carries the interposer and
//!   inherits the trace). It then judges the runs (`judge`, unit-tested over
//!   synthetic runs from both sides) and requires: every write (`setenv`,
//!   `unsetenv`, `putenv`, `clearenv`) a test makes holds the crate lock;
//!   every read a test makes of a variable some test writes (the planted
//!   variables, derived from the trace, never listed) holds it; and nothing
//!   shows the instrument failed: every test passed alone within its time
//!   limit, every test process traced a harness access before the test ran
//!   (the start canary: the harness reads at least one variable, so an empty
//!   trace means the interposer did not run) and wrote its end line after it
//!   (the end canary: a test that closes the trace's descriptor, or puts
//!   another file in its place, loses it), no trace write failed or went
//!   elsewhere (the end line's count of lines written is no more than the
//!   trace holds), every line parses, the harness wrote nothing and its
//!   accesses are exactly `HARNESS_ACCESSES`, and the canary test made
//!   exactly the eight locked accesses it makes, one through each of the six
//!   functions.
//!
//! A read of a variable no test writes is counted and printed, not failed:
//! no plant exists in the run for it to observe. How many there are depends
//! on the environment the driver starts from, whose snapshot every test
//! process inherits, so the figure the driver prints is a fact about one run,
//! not about the crate.
//!
//! What a pass does not show: it covers the code the lib unit tests execute
//! on Linux with glibc with the features of the build, and nothing else (the
//! integration tests under `tests/`, whose binaries carry no interposer;
//! other features; other platforms and C libraries, where this module is not
//! compiled). Each test runs alone, so the check reports where a race with
//! another test's plant could happen (a read of a planted variable without
//! the lock), not that one did. An access while another thread of the test
//! holds the lock counts as held: the holder is read at the access, so it is
//! a sample, not an order, and the other thread may release the lock the
//! instant after. A thread of the test still running when the process exits
//! is cut off: what it would have accessed after the exit is never traced,
//! and a line it writes after the end line is outside the end line's count.
//! A process the test starts that runs another program (`sh`, `git`) carries
//! no interposer and is not traced. The environment a spawned child inherits
//! is read from `environ` by the standard library, as `std::env::vars` and
//! `vars_os` are, and none of these goes through the interposed functions, so
//! none is traced; nor is code that calls the C library's own internal getter
//! rather than the `getenv` symbol. The instrument's own reads are not traced:
//! the constructor reads `SQRY_DAEMON_ENV_TRACE` through the C library
//! directly, and the driver takes its snapshot in a process that is not
//! traced, under the lock.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{CStr, OsString, c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicI32, AtomicPtr, AtomicU64, Ordering};
use std::time::Duration;

/// The variable naming the file the trace appends to.
const TRACE_VARIABLE: &str = "SQRY_DAEMON_ENV_TRACE";

/// The trace file's descriptor, or -1 when the trace is off.
static TRACE_FD: AtomicI32 = AtomicI32::new(-1);

/// The trace lines this process wrote whole, and the writes that failed or
/// were short (`record`), for the end line (`end_trace`).
static LINES_WRITTEN: AtomicU64 = AtomicU64::new(0);
static WRITES_FAILED: AtomicU64 = AtomicU64::new(0);

/// The C library's functions, found once.
static REAL_GETENV: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static REAL_SECURE_GETENV: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static REAL_SETENV: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static REAL_UNSETENV: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static REAL_PUTENV: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static REAL_CLEARENV: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// The C library's definition of `name`, the next one after this binary's
/// (`dlsym(RTLD_NEXT, ..)`), found once; a lookup that finds nothing aborts
/// (`missing`).
fn real(slot: &AtomicPtr<c_void>, name: &CStr) -> *mut c_void {
    real_with(slot, name, next_definition, missing)
}

/// `real` with its lookup and its failure given, so a test can make the
/// lookup find nothing: never null, it returns what `lookup` found and keeps
/// it in `slot`, or calls `fail` when `lookup` finds nothing.
fn real_with(
    slot: &AtomicPtr<c_void>,
    name: &CStr,
    lookup: fn(&CStr) -> *mut c_void,
    fail: fn(&CStr) -> !,
) -> *mut c_void {
    let found = slot.load(Ordering::Acquire);
    if !found.is_null() {
        return found;
    }
    let found = lookup(name);
    if found.is_null() {
        fail(name);
    }
    slot.store(found, Ordering::Release);
    found
}

/// `dlsym(RTLD_NEXT, name)`.
fn next_definition(name: &CStr) -> *mut c_void {
    // SAFETY: `dlsym` with `RTLD_NEXT` and a NUL-terminated name has no
    // other precondition.
    unsafe { libc::dlsym(libc::RTLD_NEXT, name.as_ptr()) }
}

/// Writes `missing_message` to standard error and aborts, without
/// allocating: it may run inside `getenv`, or before `main`.
fn missing(name: &CStr) -> ! {
    let mut message = [0u8; 256];
    let length = missing_message(&mut message, name.to_bytes());
    // SAFETY: `message[..length]` is initialised; `write` and `abort` are
    // async-signal-safe and need nothing else.
    unsafe {
        libc::write(2, message.as_ptr().cast(), length);
        libc::abort()
    }
}

/// The line `missing` writes, the name cut to fit the buffer. Returns its
/// length.
fn missing_message(buffer: &mut [u8; 256], name: &[u8]) -> usize {
    let mut length = 0usize;
    let mut push = |bytes: &[u8]| {
        for byte in bytes {
            if length < buffer.len() - 1 {
                buffer[length] = *byte;
                length += 1;
            }
        }
    };
    push(b"env_trace: dlsym(RTLD_NEXT, ");
    push(name);
    push(b") found no definition, so the interposer cannot forward; aborting");
    buffer[length] = b'\n';
    length + 1
}

type Getter = unsafe extern "C" fn(*const c_char) -> *mut c_char;
type Setter = unsafe extern "C" fn(*const c_char, *const c_char, c_int) -> c_int;
type Unsetter = unsafe extern "C" fn(*const c_char) -> c_int;
type Putter = unsafe extern "C" fn(*mut c_char) -> c_int;
type Clearer = unsafe extern "C" fn() -> c_int;

/// Runs before `main`: finds the C library's functions (aborting when one is
/// missing) and, when the trace variable names a file, opens it for
/// appending.
extern "C" fn start_trace() {
    let getenv = real(&REAL_GETENV, c"getenv");
    for (slot, name) in [
        (&REAL_SECURE_GETENV, c"secure_getenv"),
        (&REAL_SETENV, c"setenv"),
        (&REAL_UNSETENV, c"unsetenv"),
        (&REAL_PUTENV, c"putenv"),
        (&REAL_CLEARENV, c"clearenv"),
    ] {
        real(slot, name);
    }
    // SAFETY: `getenv` is the C library's `getenv`, whose signature is
    // `Getter`.
    let getenv: Getter = unsafe { std::mem::transmute::<*mut c_void, Getter>(getenv) };
    let variable = c"SQRY_DAEMON_ENV_TRACE";
    // SAFETY: a NUL-terminated name; no other thread runs before `main`.
    let path = unsafe { getenv(variable.as_ptr()) };
    if path.is_null() {
        return;
    }
    // SAFETY: `path` is the NUL-terminated value `getenv` returned.
    let fd = unsafe {
        libc::open(
            path,
            libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND | libc::O_CLOEXEC,
            0o600,
        )
    };
    TRACE_FD.store(fd, Ordering::Release);
}

#[used]
#[unsafe(link_section = ".init_array")]
static START_TRACE: extern "C" fn() = start_trace;

/// Runs at exit: appends the end line (`encode_end`), when the trace is on.
/// Its own `write` is not checked: a line that does not arrive is what the
/// driver looks for.
extern "C" fn end_trace() {
    let fd = TRACE_FD.load(Ordering::Acquire);
    if fd < 0 {
        return;
    }
    // SAFETY: `getpid` has no preconditions and cannot fail.
    let pid = unsafe { libc::getpid() };
    let mut line = [0u8; 640];
    let length = encode_end(
        &mut line,
        u64::try_from(pid).unwrap_or(0),
        LINES_WRITTEN.load(Ordering::SeqCst),
        WRITES_FAILED.load(Ordering::SeqCst),
    );
    // SAFETY: `line[..length]` is initialised.
    unsafe {
        libc::write(fd, line.as_ptr().cast(), length);
    }
}

#[used]
#[unsafe(link_section = ".fini_array")]
static END_TRACE: extern "C" fn() = end_trace;

/// Appends one access to the trace, when the trace is on (`encode_line`),
/// and counts whether the line was written whole.
fn record(operation: &[u8], name: &[u8]) {
    let fd = TRACE_FD.load(Ordering::Acquire);
    if fd < 0 {
        return;
    }
    // SAFETY: `getpid` and `gettid` have no preconditions and cannot fail.
    let (pid, tid) = unsafe { (libc::getpid(), libc::gettid()) };
    let mut line = [0u8; 640];
    let length = encode_line(
        &mut line,
        operation,
        u64::try_from(pid).unwrap_or(0),
        u64::try_from(tid).unwrap_or(0),
        crate::TEST_ENV_LOCK.holder(),
        name,
    );
    // SAFETY: `line[..length]` is initialised.
    let written = unsafe { libc::write(fd, line.as_ptr().cast(), length) };
    tally(written, length, &LINES_WRITTEN, &WRITES_FAILED);
}

/// Counts one `write` of a `length`-byte line that returned `written`: a line
/// written whole in `whole`, a short or failed write in `failed`.
fn tally(written: isize, length: usize, whole: &AtomicU64, failed: &AtomicU64) {
    if usize::try_from(written).is_ok_and(|written| written == length) {
        whole.fetch_add(1, Ordering::SeqCst);
    } else {
        failed.fetch_add(1, Ordering::SeqCst);
    }
}

/// The end line `end_trace` writes: `end`, the process id, the lines this
/// process wrote whole and the writes that failed or were short, tab
/// separated and ended by a newline. Returns the length.
fn encode_end(line: &mut [u8; 640], pid: u64, written: u64, failed: u64) -> usize {
    let mut length = 0usize;
    let mut push = |bytes: &[u8]| {
        for byte in bytes {
            if length < line.len() - 1 {
                line[length] = *byte;
                length += 1;
            }
        }
    };
    let mut number = [0u8; 24];
    push(b"end\t");
    push(decimal(pid, &mut number));
    push(b"\t");
    push(decimal(written, &mut number));
    push(b"\t");
    push(decimal(failed, &mut number));
    line[length] = b'\n';
    length + 1
}

/// One trace line, written into `line` without allocating (it runs inside
/// `getenv`): the operation, the process and thread ids, the crate lock's
/// holder and the name, tab separated and ended by a newline, the name with
/// every byte outside printable ASCII (and the tab and the backslash) written
/// as `\xHH`. A name too long for the buffer is cut. Returns the length.
fn encode_line(
    line: &mut [u8; 640],
    operation: &[u8],
    pid: u64,
    tid: u64,
    holder: u64,
    name: &[u8],
) -> usize {
    let mut length = 0usize;
    let mut push = |bytes: &[u8]| {
        for byte in bytes {
            if length < line.len() - 1 {
                line[length] = *byte;
                length += 1;
            }
        }
    };
    let mut number = [0u8; 24];
    push(operation);
    push(b"\t");
    push(decimal(pid, &mut number));
    push(b"\t");
    push(decimal(tid, &mut number));
    push(b"\t");
    push(decimal(holder, &mut number));
    push(b"\t");
    for byte in name {
        match byte {
            b'\\' | b'\t' => push(&hex_escape(*byte)),
            0x20..=0x7e => push(&[*byte]),
            _ => push(&hex_escape(*byte)),
        }
    }
    line[length] = b'\n';
    length + 1
}

/// `value` in decimal, written into `buffer`.
fn decimal(mut value: u64, buffer: &mut [u8; 24]) -> &[u8] {
    let mut start = buffer.len();
    loop {
        start -= 1;
        buffer[start] = b'0' + u8::try_from(value % 10).unwrap_or(0);
        value /= 10;
        if value == 0 {
            break;
        }
    }
    &buffer[start..]
}

/// `\xHH` for one byte.
fn hex_escape(byte: u8) -> [u8; 4] {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    [
        b'\\',
        b'x',
        DIGITS[usize::from(byte >> 4)],
        DIGITS[usize::from(byte & 0xf)],
    ]
}

/// The bytes of a NUL-terminated name, or nothing for a null pointer.
///
/// # Safety
///
/// `name` is null or points to a NUL-terminated string.
unsafe fn name_bytes<'a>(name: *const c_char) -> &'a [u8] {
    if name.is_null() {
        return &[];
    }
    // SAFETY: the caller's contract.
    unsafe { CStr::from_ptr(name) }.to_bytes()
}

/// `getenv(3)`, traced.
///
/// # Safety
///
/// As `getenv(3)`: `name` points to a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getenv(name: *const c_char) -> *mut c_char {
    // SAFETY: the caller's contract.
    record(b"get", unsafe { name_bytes(name) });
    let real = real(&REAL_GETENV, c"getenv");
    // SAFETY: the C library's `getenv`, called with the caller's argument.
    unsafe { std::mem::transmute::<*mut c_void, Getter>(real)(name) }
}

/// `secure_getenv(3)`, traced.
///
/// # Safety
///
/// As `secure_getenv(3)`: `name` points to a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn secure_getenv(name: *const c_char) -> *mut c_char {
    // SAFETY: the caller's contract.
    record(b"secure_get", unsafe { name_bytes(name) });
    let real = real(&REAL_SECURE_GETENV, c"secure_getenv");
    // SAFETY: the C library's `secure_getenv`, called with the caller's
    // argument.
    unsafe { std::mem::transmute::<*mut c_void, Getter>(real)(name) }
}

/// `setenv(3)`, traced.
///
/// # Safety
///
/// As `setenv(3)`: `name` and `value` point to NUL-terminated strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn setenv(
    name: *const c_char,
    value: *const c_char,
    overwrite: c_int,
) -> c_int {
    // SAFETY: the caller's contract.
    record(b"set", unsafe { name_bytes(name) });
    let real = real(&REAL_SETENV, c"setenv");
    // SAFETY: the C library's `setenv`, called with the caller's arguments.
    unsafe { std::mem::transmute::<*mut c_void, Setter>(real)(name, value, overwrite) }
}

/// `unsetenv(3)`, traced.
///
/// # Safety
///
/// As `unsetenv(3)`: `name` points to a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn unsetenv(name: *const c_char) -> c_int {
    // SAFETY: the caller's contract.
    record(b"unset", unsafe { name_bytes(name) });
    let real = real(&REAL_UNSETENV, c"unsetenv");
    // SAFETY: the C library's `unsetenv`, called with the caller's argument.
    unsafe { std::mem::transmute::<*mut c_void, Unsetter>(real)(name) }
}

/// `putenv(3)`, traced under the name before the `=`.
///
/// # Safety
///
/// As `putenv(3)`: `string` points to a NUL-terminated string that outlives
/// its place in the environment.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn putenv(string: *mut c_char) -> c_int {
    // SAFETY: the caller's contract.
    record(b"put", putenv_name(unsafe { name_bytes(string) }));
    let real = real(&REAL_PUTENV, c"putenv");
    // SAFETY: the C library's `putenv`, called with the caller's argument.
    unsafe { std::mem::transmute::<*mut c_void, Putter>(real)(string) }
}

/// The variable a `putenv` string names: what comes before its first `=`,
/// or all of it (which `putenv` reads as a removal).
fn putenv_name(string: &[u8]) -> &[u8] {
    string.split(|byte| *byte == b'=').next().unwrap_or(string)
}

/// `clearenv(3)`, traced under the name `*`.
///
/// # Safety
///
/// As `clearenv(3)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn clearenv() -> c_int {
    record(b"clear", b"*");
    let real = real(&REAL_CLEARENV, c"clearenv");
    // SAFETY: the C library's `clearenv`.
    unsafe { std::mem::transmute::<*mut c_void, Clearer>(real)() }
}

// ---------------------------------------------------------------------------
// The driver
// ---------------------------------------------------------------------------

/// The canary test's name, and the variable it reads and writes.
const CANARY_TEST: &str = "env_trace::canary_reads_and_writes_under_the_crate_lock";
const CANARY_VARIABLE: &str = "SQRY_DAEMON_ENV_TRACE_CANARY";

/// The driver's own name, which it never runs in a child.
const DRIVER_TEST: &str = "env_trace::every_environment_access_of_a_lib_test_holds_the_crate_lock";

/// Every access the test harness makes on its main thread in a process
/// running one test with `--exact <name> --test-threads=1` and no `TERM`:
/// libtest reads `RUST_TEST_NOCAPTURE` while it parses its options and `TERM`
/// while it probes the terminal (with `TERM` set it would go on to read
/// `TERMINFO`, `TERMINFO_DIRS` and `HOME`, which is why the driver removes
/// it), and the standard library reads `RUST_MIN_STACK` at the first thread
/// spawn, the test's own thread. `(operation, variable)`. The driver asserts
/// the observed set equals this one, so an entry here that is never made
/// fails as surely as an access not here.
const HARNESS_ACCESSES: [(&str, &str); 3] = [
    ("get", "RUST_MIN_STACK"),
    ("get", "RUST_TEST_NOCAPTURE"),
    ("get", "TERM"),
];

/// The variables removed from the environment the test processes run with:
/// the trace's own variable (each process gets its own file) and `TERM`
/// (`HARNESS_ACCESSES`).
const REMOVED_VARIABLES: [&str; 2] = [TRACE_VARIABLE, "TERM"];

/// The operations that write the environment.
const WRITES: [&str; 4] = ["set", "unset", "put", "clear"];

/// The `putenv` string the canary gives: it must outlive its place in the
/// environment, so it is static.
const CANARY_PUT: &CStr = c"SQRY_DAEMON_ENV_TRACE_CANARY=2";

/// How long one test may run alone under the trace before the driver stops
/// it and reports it: far above any lib test's time, so only a hang (a second
/// acquisition of the crate lock, a deadlock) reaches it.
const TEST_TIMEOUT: Duration = Duration::from_secs(300);

/// The canary: eight accesses of a variable no other test touches, all under
/// the crate lock, one through each of the six interposed functions, which
/// the driver requires to find in this test's trace in this order: `get`,
/// `set`, `get` (`std::env`), `secure_get` (`secure_getenv`, which the
/// standard library never calls, traced under a name of its own so that a
/// `getenv` in its place is told apart), `put` (`putenv`), `get`, `unset`,
/// and `clear *` (`clearenv`). `clearenv` empties the whole environment, so the canary
/// calls it only when the trace is on, in the process of its own the driver
/// runs it in, where nothing else runs and nothing reads the environment
/// after it.
#[test]
fn canary_reads_and_writes_under_the_crate_lock() {
    let _env = crate::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let before = std::env::var_os(CANARY_VARIABLE);
    // SAFETY: the crate lock serialises every environment access of the
    // crate's tests.
    unsafe { std::env::set_var(CANARY_VARIABLE, "1") };
    let during = std::env::var_os(CANARY_VARIABLE);
    // SAFETY: a NUL-terminated name, under the crate lock.
    let secure = unsafe { secure_getenv(c"SQRY_DAEMON_ENV_TRACE_CANARY".as_ptr()) };
    // SAFETY: a static NUL-terminated string, under the crate lock; `putenv`
    // does not write to it.
    let put = unsafe { putenv(CANARY_PUT.as_ptr().cast_mut()) };
    let after_put = std::env::var_os(CANARY_VARIABLE);
    // SAFETY: as above.
    unsafe { std::env::remove_var(CANARY_VARIABLE) };
    if TRACE_FD.load(Ordering::Acquire) >= 0 {
        // SAFETY: under the crate lock, alone in this traced process.
        let cleared = unsafe { clearenv() };
        assert_eq!(cleared, 0, "clearenv succeeded");
    }
    assert_eq!(before, None, "no other test plants the canary");
    assert_eq!(during, Some(OsString::from("1")), "the canary was set");
    assert!(!secure.is_null(), "secure_getenv found the canary");
    assert_eq!(put, 0, "putenv succeeded");
    assert_eq!(
        after_put,
        Some(OsString::from("2")),
        "putenv replaced the canary"
    );
}

/// One traced access.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Access {
    operation: String,
    pid: u64,
    tid: u64,
    holder: u64,
    name: String,
}

impl Access {
    fn parse(line: &str) -> Option<Self> {
        let mut fields = line.split('\t');
        let access = Self {
            operation: fields.next()?.to_string(),
            pid: fields.next()?.parse().ok()?,
            tid: fields.next()?.parse().ok()?,
            holder: fields.next()?.parse().ok()?,
            name: fields.next()?.to_string(),
        };
        fields.next().is_none().then_some(access)
    }

    fn is_write(&self) -> bool {
        WRITES.contains(&self.operation.as_str())
    }
}

/// One process's end line (`encode_end`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct TraceEnd {
    pid: u64,
    /// The trace lines the process wrote whole.
    written: u64,
    /// The writes that failed or were short.
    failed: u64,
}

impl TraceEnd {
    fn parse(line: &str) -> Option<Self> {
        let mut fields = line.split('\t');
        if fields.next()? != "end" {
            return None;
        }
        let end = Self {
            pid: fields.next()?.parse().ok()?,
            written: fields.next()?.parse().ok()?,
            failed: fields.next()?.parse().ok()?,
        };
        fields.next().is_none().then_some(end)
    }
}

/// One test run alone under the trace.
#[derive(Debug, Clone)]
struct TracedRun {
    test: String,
    /// The process libtest ran in.
    pid: u64,
    /// The run passed: libtest ran one test and it passed (`passed_alone`).
    passed: bool,
    /// The run was stopped at `TEST_TIMEOUT`.
    timed_out: bool,
    accesses: Vec<Access>,
    /// The end lines, one per process that exited and wrote one.
    ends: Vec<TraceEnd>,
    /// The lines that parse as neither an access nor an end line.
    unparsed: Vec<String>,
}

impl TracedRun {
    /// A run's trace, line by line.
    fn from_trace(test: &str, pid: u64, passed: bool, timed_out: bool, text: &str) -> Self {
        let mut run = TracedRun {
            test: test.to_string(),
            pid,
            passed,
            timed_out,
            accesses: Vec::new(),
            ends: Vec::new(),
            unparsed: Vec::new(),
        };
        for line in text.lines() {
            if let Some(end) = TraceEnd::parse(line) {
                run.ends.push(end);
            } else if let Some(access) = Access::parse(line) {
                run.accesses.push(access);
            } else {
                run.unparsed.push(line.to_string());
            }
        }
        run
    }
}

/// Whether libtest's output says it ran one test and that it passed.
fn passed_alone(success: bool, stdout: &str) -> bool {
    success && stdout.contains("running 1 test") && stdout.contains("test result: ok. 1 passed;")
}

/// What the traces say (`judge`).
#[derive(Debug, Default)]
struct Judgement {
    /// Every write a test made without the lock, and every read without it
    /// of a planted variable, as `test: operation variable`.
    violations: Vec<String>,
    /// Every way the runs show the instrument did not do its job.
    instrument_failures: Vec<String>,
    /// The variables some test writes.
    planted: BTreeSet<String>,
    /// The harness's accesses, `(operation, variable)`.
    harness: BTreeSet<(String, String)>,
    /// Reads without the lock of a variable no test writes, by variable.
    unplanted_unlocked: BTreeMap<String, usize>,
    trace_lines: usize,
    test_accesses: usize,
    held_by_the_thread: usize,
    held_by_another: usize,
    started_processes: usize,
    /// The end lines read, one per process that wrote one.
    end_lines: usize,
    /// A test other than the canary cleared the environment, so every
    /// variable is planted.
    everything_planted: bool,
}

/// The canary's accesses, in order, as `(operation, variable)`: see
/// `canary_reads_and_writes_under_the_crate_lock`.
fn expected_canary(canary_variable: &str) -> Vec<(&str, &str)> {
    vec![
        ("get", canary_variable),
        ("set", canary_variable),
        ("get", canary_variable),
        ("secure_get", canary_variable),
        ("put", canary_variable),
        ("get", canary_variable),
        ("unset", canary_variable),
        ("clear", "*"),
    ]
}

/// Decides what the traced runs show. In a run's own process its main thread
/// (thread id equal to the process id) is the harness, every other thread and
/// every other process the test. The planted variables are the ones some
/// test writes, and every variable once a test other than the canary clears
/// the environment. A violation is a test's write without the lock, or a test's
/// read without the lock of a planted variable; an access is held when the
/// lock's recorded holder is a thread (the accessing one, or another of the
/// test's). An instrument failure is a run that did not pass alone or was
/// stopped at its time limit, a run that traced no harness access (the
/// interposer did not run) or no end line of its own process (the end canary:
/// the descriptor was closed or replaced, or the process never exited), an
/// end line that counts a failed write or more lines than the trace holds
/// for its process (lines written elsewhere), a line that does not parse, a
/// harness write, a harness access set other than `harness_allowlist` (an
/// access missing from it, or an entry never made), and a canary other than
/// `canary_test`'s eight locked accesses (`expected_canary`), in order.
fn judge(
    runs: &[TracedRun],
    harness_allowlist: &BTreeSet<(String, String)>,
    canary_test: &str,
    canary_variable: &str,
) -> Judgement {
    let mut judgement = Judgement::default();
    let mut by_test: BTreeMap<&str, Vec<&Access>> = BTreeMap::new();
    for run in runs {
        if run.timed_out {
            judgement.instrument_failures.push(format!(
                "{} did not finish within {} s and was stopped",
                run.test,
                TEST_TIMEOUT.as_secs()
            ));
        }
        if !run.passed {
            judgement
                .instrument_failures
                .push(format!("{} did not pass when run alone", run.test));
        }
        for line in &run.unparsed {
            judgement.instrument_failures.push(format!(
                "a trace line of {} does not parse: {line:?}",
                run.test
            ));
        }
        judgement.trace_lines += run.accesses.len();
        judgement.end_lines += run.ends.len();
        let mut lines_by_process: BTreeMap<u64, u64> = BTreeMap::new();
        for access in &run.accesses {
            *lines_by_process.entry(access.pid).or_default() += 1;
        }
        if !run.ends.iter().any(|end| end.pid == run.pid) {
            judgement.instrument_failures.push(format!(
                "{}'s process wrote no end line, so the trace's descriptor was closed or \
                 replaced, or the process did not exit",
                run.test
            ));
        }
        for end in &run.ends {
            let traced = lines_by_process.get(&end.pid).copied().unwrap_or(0);
            if end.failed > 0 {
                judgement.instrument_failures.push(format!(
                    "process {} of {}'s run failed {} trace writes",
                    end.pid, run.test, end.failed
                ));
            }
            if traced < end.written {
                judgement.instrument_failures.push(format!(
                    "process {} of {}'s run wrote {} trace lines and the trace holds {traced}",
                    end.pid, run.test, end.written
                ));
            }
        }
        let mut harness_lines = 0usize;
        let mut started: BTreeSet<u64> = BTreeSet::new();
        for access in &run.accesses {
            if access.pid == run.pid && access.tid == run.pid {
                harness_lines += 1;
                if access.is_write() {
                    judgement.instrument_failures.push(format!(
                        "the harness wrote {} in {}'s run",
                        access.name, run.test
                    ));
                }
                judgement
                    .harness
                    .insert((access.operation.clone(), access.name.clone()));
            } else {
                if access.pid != run.pid {
                    started.insert(access.pid);
                }
                by_test.entry(run.test.as_str()).or_default().push(access);
            }
        }
        judgement.started_processes += started.len();
        if harness_lines == 0 {
            judgement.instrument_failures.push(format!(
                "{}'s process traced no harness access, so the interposer did not run",
                run.test
            ));
        }
    }
    if judgement.harness != *harness_allowlist {
        judgement.instrument_failures.push(format!(
            "the harness's accesses {:?} are not the allowlist {harness_allowlist:?}",
            judgement.harness
        ));
    }
    judgement.planted = by_test
        .values()
        .flatten()
        .filter(|access| access.is_write())
        .map(|access| access.name.clone())
        .collect();
    // A `clearenv` plants the absence of every variable. The canary's own
    // runs only under the trace, alone in its process, so it plants nothing
    // another test could observe.
    judgement.everything_planted = by_test.iter().any(|(test, accesses)| {
        *test != canary_test && accesses.iter().any(|access| access.operation == "clear")
    });
    for (test, accesses) in &by_test {
        for access in accesses {
            judgement.test_accesses += 1;
            if access.holder == access.tid {
                judgement.held_by_the_thread += 1;
            } else if access.holder != 0 {
                judgement.held_by_another += 1;
            } else if access.is_write()
                || judgement.everything_planted
                || judgement.planted.contains(&access.name)
            {
                judgement
                    .violations
                    .push(format!("{test}: {} {}", access.operation, access.name));
            } else {
                *judgement
                    .unplanted_unlocked
                    .entry(access.name.clone())
                    .or_default() += 1;
            }
        }
    }
    judgement.violations.sort();
    judgement.violations.dedup();
    let canary: Vec<(&str, &str, bool)> = by_test
        .get(canary_test)
        .into_iter()
        .flatten()
        .map(|access| {
            (
                access.operation.as_str(),
                access.name.as_str(),
                access.holder == access.tid,
            )
        })
        .collect();
    let expected_canary: Vec<(&str, &str, bool)> = expected_canary(canary_variable)
        .into_iter()
        .map(|(operation, name)| (operation, name, true))
        .collect();
    if canary != expected_canary {
        judgement.instrument_failures.push(format!(
            "the canary's accesses are {canary:?}, not its eight locked ones"
        ));
    }
    judgement
}

/// The environment the test processes run with, and the directory their
/// traces go to, both taken under the crate lock.
struct TraceRun {
    environment: Vec<(OsString, OsString)>,
    directory: tempfile::TempDir,
}

fn prepare() -> TraceRun {
    let _env = crate::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let environment = std::env::vars_os()
        .filter(|(name, _)| !REMOVED_VARIABLES.iter().any(|removed| name == removed))
        .collect();
    let directory = tempfile::tempdir().expect("a directory for the traces");
    TraceRun {
        environment,
        directory,
    }
}

/// This binary's tests, as `--list` prints them (`--ignored` lists only the
/// ignored ones).
fn list_tests(exe: &Path, run: &TraceRun, ignored: bool) -> BTreeSet<String> {
    let mut command = Command::new(exe);
    command
        .env_clear()
        .envs(run.environment.iter().map(|(name, value)| (name, value)))
        .args(["--list", "--format", "terse"]);
    if ignored {
        command.arg("--ignored");
    }
    let output = command.output().expect("list the tests");
    assert!(
        output.status.success(),
        "instrument: listing the tests failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.strip_suffix(": test"))
        .map(str::to_string)
        .collect()
}

/// Runs one test in a process of its own with the trace on, its output to
/// files, and stops it at `TEST_TIMEOUT`.
fn run_traced(exe: &Path, run: &TraceRun, index: usize, name: &str) -> TracedRun {
    let directory = run.directory.path();
    let trace: PathBuf = directory.join(format!("{index}.trace"));
    let stdout_path = directory.join(format!("{index}.stdout"));
    let stderr_path = directory.join(format!("{index}.stderr"));
    let mut child = Command::new(exe)
        .env_clear()
        .envs(run.environment.iter().map(|(name, value)| (name, value)))
        .env(TRACE_VARIABLE, &trace)
        .args(["--exact", name, "--test-threads=1"])
        .stdout(std::fs::File::create(&stdout_path).expect("the test's stdout file"))
        .stderr(std::fs::File::create(&stderr_path).expect("the test's stderr file"))
        .spawn()
        .expect("start the traced test");
    let pid = u64::from(child.id());
    let (status, timed_out) = wait_or_stop(&mut child, TEST_TIMEOUT);
    let stdout = std::fs::read_to_string(&stdout_path).unwrap_or_default();
    let passed = !timed_out && passed_alone(status.success(), &stdout);
    if !passed {
        println!(
            "the traced run of {name} did not pass alone{}:\n{stdout}\n{}",
            if timed_out {
                " (stopped at its time limit)"
            } else {
                ""
            },
            std::fs::read_to_string(&stderr_path).unwrap_or_default()
        );
    }
    let text = std::fs::read_to_string(&trace).unwrap_or_default();
    TracedRun::from_trace(name, pid, passed, timed_out, &text)
}

/// The tests the driver runs traced: every listed test that `cargo test`
/// runs, so not an ignored one, and never the driver itself.
fn tests_to_run<'a>(listed: &'a BTreeSet<String>, ignored: &BTreeSet<String>) -> Vec<&'a String> {
    listed
        .iter()
        .filter(|name| !ignored.contains(*name) && name.as_str() != DRIVER_TEST)
        .collect()
}

/// Waits for `child`, and stops it once `limit` has passed: its exit status,
/// and whether it was stopped. It polls, with a pause that grows to 20 ms, so
/// a test that ends at once costs a millisecond.
fn wait_or_stop(
    child: &mut std::process::Child,
    limit: Duration,
) -> (std::process::ExitStatus, bool) {
    let started = std::time::Instant::now();
    let mut pause = Duration::from_millis(1);
    loop {
        if let Some(status) = child.try_wait().expect("poll the child") {
            return (status, false);
        }
        if started.elapsed() >= limit {
            child.kill().expect("stop the child");
            return (child.wait().expect("wait for the stopped child"), true);
        }
        std::thread::sleep(pause);
        pause = (pause * 2).min(Duration::from_millis(20));
    }
}

/// The runtime check (see the module doc).
#[test]
fn every_environment_access_of_a_lib_test_holds_the_crate_lock() {
    assert!(
        TRACE_FD.load(Ordering::Acquire) < 0,
        "instrument: the driver cannot run under its own trace"
    );
    let run = prepare();
    let exe = std::env::current_exe().expect("this test binary's path");
    let listed = list_tests(&exe, &run, false);
    let ignored = list_tests(&exe, &run, true);
    let tests = tests_to_run(&listed, &ignored);
    println!(
        "lib tests listed {}, ignored {}, run traced {} (the driver itself excluded)",
        listed.len(),
        ignored.len(),
        tests.len()
    );
    assert!(
        listed.contains(DRIVER_TEST) && listed.contains(CANARY_TEST),
        "instrument: the listing names the driver and the canary"
    );
    assert_eq!(
        tests.len() + ignored.len() + 1,
        listed.len(),
        "instrument: every listed test is run, ignored, or the driver"
    );
    let runs: Vec<TracedRun> = tests
        .iter()
        .enumerate()
        .map(|(index, name)| run_traced(&exe, &run, index, name))
        .collect();
    let allowlist: BTreeSet<(String, String)> = HARNESS_ACCESSES
        .iter()
        .map(|(operation, name)| ((*operation).to_string(), (*name).to_string()))
        .collect();
    let judgement = judge(&runs, &allowlist, CANARY_TEST, CANARY_VARIABLE);

    println!(
        "trace lines {}: harness {}, tests {}",
        judgement.trace_lines,
        judgement.trace_lines - judgement.test_accesses,
        judgement.test_accesses
    );
    println!(
        "test accesses: held by the accessing thread {}, held by another thread {}, unlocked {}",
        judgement.held_by_the_thread,
        judgement.held_by_another,
        judgement.test_accesses - judgement.held_by_the_thread - judgement.held_by_another
    );
    println!(
        "processes a test started that traced an access: {}; end lines read: {} (one per \
         process that exited and wrote one)",
        judgement.started_processes, judgement.end_lines
    );
    println!(
        "planted variables (written by some test): {}",
        judgement.planted.len()
    );
    for name in &judgement.planted {
        println!("  {name}");
    }
    println!(
        "unlocked reads of variables no test writes: {} variables, {} reads",
        judgement.unplanted_unlocked.len(),
        judgement.unplanted_unlocked.values().sum::<usize>()
    );
    for (name, count) in &judgement.unplanted_unlocked {
        println!("  {name}: {count}");
    }
    println!("harness accesses: {:?}", judgement.harness);
    println!(
        "unlocked writes, or unlocked reads of a planted variable: {}",
        judgement.violations.len()
    );
    for violation in &judgement.violations {
        println!("  {violation}");
    }
    println!(
        "instrument failures: {}",
        judgement.instrument_failures.len()
    );
    for failure in &judgement.instrument_failures {
        println!("  {failure}");
    }
    assert!(
        judgement.instrument_failures.is_empty(),
        "instrument: {:#?}",
        judgement.instrument_failures
    );
    assert!(
        judgement.violations.is_empty(),
        "every environment write a lib test makes, and every read of a variable a test \
         writes, holds crate::TEST_ENV_LOCK: {:#?}",
        judgement.violations
    );
}

/// The judgement, the trace's encoding and the interposer's lookup, from both
/// sides, over synthetic runs and injected lookups: each rule reports what it
/// should and nothing else.
#[cfg(test)]
mod judge_tests {
    use super::{
        Access, TEST_TIMEOUT, TraceEnd, TracedRun, encode_end, encode_line, expected_canary, judge,
        missing_message, passed_alone, putenv_name, real_with, tally, tests_to_run, wait_or_stop,
    };
    use std::collections::BTreeSet;
    use std::ffi::{CStr, c_void};
    use std::sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering};
    use std::time::Duration;

    const CANARY_TEST: &str = "canary";
    const CANARY: &str = "CANARY";

    fn access(operation: &str, pid: u64, tid: u64, holder: u64, name: &str) -> Access {
        Access {
            operation: operation.to_string(),
            pid,
            tid,
            holder,
            name: name.to_string(),
        }
    }

    /// A run that passed, with the harness's start access, the accesses
    /// given, and an end line for each process that counts its lines.
    fn run(test: &str, pid: u64, accesses: Vec<Access>) -> TracedRun {
        let mut all = vec![access("get", pid, pid, 0, "RUST_MIN_STACK")];
        all.extend(accesses);
        let mut pids: BTreeSet<u64> = all.iter().map(|access| access.pid).collect();
        pids.insert(pid);
        let ends = pids
            .into_iter()
            .map(|process| TraceEnd {
                pid: process,
                written: all.iter().filter(|access| access.pid == process).count() as u64,
                failed: 0,
            })
            .collect();
        TracedRun {
            test: test.to_string(),
            pid,
            passed: true,
            timed_out: false,
            accesses: all,
            ends,
            unparsed: Vec::new(),
        }
    }

    fn canary(pid: u64) -> TracedRun {
        let tid = pid + 1;
        run(
            CANARY_TEST,
            pid,
            expected_canary(CANARY)
                .into_iter()
                .map(|(operation, name)| access(operation, pid, tid, tid, name))
                .collect(),
        )
    }

    fn allowlist() -> BTreeSet<(String, String)> {
        BTreeSet::from([("get".to_string(), "RUST_MIN_STACK".to_string())])
    }

    #[test]
    fn a_clean_set_of_runs_is_judged_clean() {
        let runs = vec![
            canary(10),
            // A locked write, an unlocked read of a variable nobody writes, a
            // read another thread of the test holds the lock for.
            run(
                "quiet",
                20,
                vec![
                    access("set", 20, 21, 21, "PLANTED"),
                    access("get", 20, 21, 0, "UNWRITTEN"),
                    access("get", 20, 22, 21, "PLANTED"),
                ],
            ),
        ];
        let judgement = judge(&runs, &allowlist(), CANARY_TEST, CANARY);
        assert!(
            judgement.violations.is_empty(),
            "{:?}",
            judgement.violations
        );
        assert!(
            judgement.instrument_failures.is_empty(),
            "{:?}",
            judgement.instrument_failures
        );
        assert_eq!(
            judgement.planted,
            BTreeSet::from(["CANARY".to_string(), "PLANTED".to_string(), "*".to_string()])
        );
        assert_eq!(judgement.unplanted_unlocked.get("UNWRITTEN"), Some(&1));
        assert_eq!(
            (
                judgement.test_accesses,
                judgement.held_by_the_thread,
                judgement.held_by_another,
                judgement.end_lines
            ),
            (11, 9, 1, 2)
        );
    }

    #[test]
    fn every_unlocked_write_and_planted_read_is_a_violation() {
        let runs = vec![
            canary(10),
            run("writer", 20, vec![access("unset", 20, 21, 0, "W")]),
            run("planter", 30, vec![access("set", 30, 31, 31, "P")]),
            // The same unlocked read twice is one violation.
            run(
                "reader",
                40,
                vec![access("get", 40, 41, 0, "P"), access("get", 40, 41, 0, "P")],
            ),
            // A process the test started reads P without the lock, on two
            // of its threads: one process started.
            run(
                "starter",
                50,
                vec![access("get", 99, 99, 0, "P"), access("get", 99, 98, 0, "P")],
            ),
            run("putter", 60, vec![access("put", 60, 61, 0, "Q")]),
            // A variable only ever removed is planted too.
            run("remover", 80, vec![access("unset", 80, 81, 81, "U")]),
            run("unset_reader", 90, vec![access("get", 90, 91, 0, "U")]),
        ];
        // An unlocked clear is a violation of its own.
        let clearer = judge(
            &[
                canary(10),
                run("clearer", 70, vec![access("clear", 70, 71, 0, "*")]),
            ],
            &allowlist(),
            CANARY_TEST,
            CANARY,
        );
        assert_eq!(clearer.violations, vec!["clearer: clear *".to_string()]);
        // A test that clears the environment, even under the lock, plants
        // every variable; the canary's own clear does not.
        let clean = judge(
            &[
                canary(10),
                run("unwritten_reader", 20, vec![access("get", 20, 21, 0, "Z")]),
            ],
            &allowlist(),
            CANARY_TEST,
            CANARY,
        );
        assert!(clean.violations.is_empty(), "{:?}", clean.violations);
        let cleared = judge(
            &[
                canary(10),
                run("locked_clearer", 30, vec![access("clear", 30, 31, 31, "*")]),
                run("unwritten_reader", 20, vec![access("get", 20, 21, 0, "Z")]),
            ],
            &allowlist(),
            CANARY_TEST,
            CANARY,
        );
        assert_eq!(
            cleared.violations,
            vec!["unwritten_reader: get Z".to_string()]
        );
        let judgement = judge(&runs, &allowlist(), CANARY_TEST, CANARY);
        assert_eq!(
            judgement.violations,
            vec![
                "putter: put Q".to_string(),
                "reader: get P".to_string(),
                "starter: get P".to_string(),
                "unset_reader: get U".to_string(),
                "writer: unset W".to_string(),
            ]
        );
        assert_eq!(
            judgement.started_processes, 1,
            "processes are counted by process id, not by thread"
        );
        assert!(
            judgement.instrument_failures.is_empty(),
            "{:?}",
            judgement.instrument_failures
        );
    }

    #[test]
    fn a_broken_instrument_is_reported() {
        let mut failed = run("failed", 20, Vec::new());
        failed.passed = false;
        let silent = TracedRun {
            test: "silent".to_string(),
            pid: 30,
            passed: true,
            timed_out: false,
            accesses: Vec::new(),
            ends: vec![TraceEnd {
                pid: 30,
                written: 0,
                failed: 0,
            }],
            unparsed: Vec::new(),
        };
        let writer = run("harness_writer", 40, vec![access("set", 40, 40, 0, "H")]);
        let stranger = run("stranger", 50, vec![access("get", 50, 50, 0, "TERM")]);
        let mut wrong_canary = canary(60);
        wrong_canary.accesses[2].holder = 0;
        let judgement = judge(
            &[failed, silent, writer, stranger, wrong_canary],
            &allowlist(),
            CANARY_TEST,
            CANARY,
        );
        let failures = judgement.instrument_failures.join("\n");
        for needle in [
            "failed did not pass when run alone",
            "silent's process traced no harness access",
            "the harness wrote H",
            "are not the allowlist",
            "the canary's accesses",
        ] {
            assert!(failures.contains(needle), "{needle} not in:\n{failures}");
        }
        assert_eq!(judgement.instrument_failures.len(), 5, "{failures}");

        // An allowlist entry never made is reported as surely as an access
        // not on it.
        let mut wider = allowlist();
        wider.insert(("get".to_string(), "NEVER".to_string()));
        let judgement = judge(&[canary(10)], &wider, CANARY_TEST, CANARY);
        assert_eq!(judgement.instrument_failures.len(), 1);
    }

    #[test]
    fn a_lost_or_failed_trace_line_is_reported() {
        // The descriptor was closed or replaced: no end line of the process.
        let mut no_end = run("no_end", 20, Vec::new());
        no_end.ends.clear();
        // A write failed or was short.
        let mut failed_write = run("failed_write", 30, Vec::new());
        failed_write.ends[0].failed = 2;
        // Lines written whole went elsewhere: the end line counts more than
        // the trace holds.
        let mut elsewhere = run("elsewhere", 40, vec![access("get", 40, 41, 41, "X")]);
        elsewhere.ends[0].written += 3;
        // A line that is neither an access nor an end line.
        let mut garbled = run("garbled", 50, Vec::new());
        garbled.unparsed.push("get\t5".to_string());
        // A run stopped at its time limit.
        let mut stopped = run("stopped", 60, Vec::new());
        stopped.timed_out = true;
        stopped.passed = false;
        // A started process with no end line is not required to write one,
        // and lines a thread writes after the end line are no loss.
        let mut late = run(
            "late",
            70,
            vec![
                access("get", 71, 71, 71, "X"),
                access("get", 70, 72, 72, "X"),
            ],
        );
        late.ends.retain(|end| end.pid == 70);
        late.ends[0].written -= 1;
        let judgement = judge(
            &[
                no_end,
                failed_write,
                elsewhere,
                garbled,
                stopped,
                late,
                canary(10),
            ],
            &allowlist(),
            CANARY_TEST,
            CANARY,
        );
        let failures = judgement.instrument_failures.join("\n");
        for needle in [
            "no_end's process wrote no end line",
            "process 30 of failed_write's run failed 2 trace writes",
            "process 40 of elsewhere's run wrote 5 trace lines and the trace holds 2",
            "a trace line of garbled does not parse",
            &format!("stopped did not finish within {} s", TEST_TIMEOUT.as_secs()),
            "stopped did not pass when run alone",
        ] {
            assert!(failures.contains(needle), "{needle} not in:\n{failures}");
        }
        assert_eq!(judgement.instrument_failures.len(), 6, "{failures}");
    }

    #[test]
    fn the_canary_must_hold_the_lock_itself_in_order() {
        // Held by another thread of the test is not the canary's own hold.
        let mut by_another = canary(10);
        by_another.accesses[1].holder = 99;
        // The right accesses in another order.
        let mut reordered = canary(10);
        reordered.accesses.swap(1, 2);
        // A `getenv` where the canary calls `secure_getenv`.
        let mut plain_get = canary(10);
        let secure = plain_get
            .accesses
            .iter_mut()
            .find(|access| access.operation == "secure_get")
            .expect("the canary's secure_getenv");
        secure.operation = "get".to_string();
        for (case, run) in [
            ("by another", by_another),
            ("reordered", reordered),
            ("a getenv for the secure_getenv", plain_get),
        ] {
            let judgement = judge(&[run], &allowlist(), CANARY_TEST, CANARY);
            assert!(
                judgement
                    .instrument_failures
                    .iter()
                    .any(|failure| failure.contains("the canary's accesses")),
                "{case}: {:?}",
                judgement.instrument_failures
            );
        }
    }

    #[test]
    fn libtest_output_passes_only_for_one_passing_test() {
        let ok = "running 1 test\ntest x ... ok\n\ntest result: ok. 1 passed; 0 failed";
        assert!(passed_alone(true, ok));
        assert!(!passed_alone(false, ok));
        assert!(!passed_alone(
            true,
            "running 0 tests\n\ntest result: ok. 0 passed;"
        ));
        assert!(!passed_alone(
            true,
            "running 1 test\ntest result: FAILED. 0 passed; 1 failed"
        ));
        assert!(!passed_alone(
            true,
            "running 1 test\ntest x ... ignored\n\ntest result: ok. 0 passed; 0 failed; 1 ignored"
        ));
        assert!(!passed_alone(
            true,
            "running 2 tests\ntest a ... ok\ntest b ... ignored\n\ntest result: ok. 1 passed; 0 failed; 1 ignored"
        ));
    }

    #[test]
    fn a_trace_line_round_trips_and_escapes_its_name() {
        let mut line = [0u8; 640];
        let length = encode_line(&mut line, b"get", 12, 34, 56, b"A\tB\\C\xffD\x7fE");
        let text = std::str::from_utf8(&line[..length]).expect("ASCII");
        assert_eq!(text, "get\t12\t34\t56\tA\\x09B\\x5cC\\xffD\\x7fE\n");
        let parsed = Access::parse(text.trim_end()).expect("a line");
        assert_eq!(
            (
                parsed.operation.as_str(),
                parsed.pid,
                parsed.tid,
                parsed.holder
            ),
            ("get", 12, 34, 56)
        );
        assert_eq!(parsed.name, "A\\x09B\\x5cC\\xffD\\x7fE");
        assert!(Access::parse("get\t1\t2\t3").is_none(), "a field short");
        assert!(
            Access::parse("get\t1\t2\t3\tN\textra").is_none(),
            "a field over"
        );
        assert!(
            Access::parse("get\tx\t2\t3\tN").is_none(),
            "a pid that is no number"
        );
        assert!(
            Access::parse("get\t1\t2\tx\tN").is_none(),
            "a holder that is no number"
        );
        let long = [b'L'; 1000];
        let length = encode_line(&mut line, b"set", 0, 0, 0, &long);
        assert_eq!(length, 640, "a long name is cut to the buffer");
        assert_eq!(line[639], b'\n');
    }

    #[test]
    fn an_end_line_round_trips_and_is_told_from_an_access() {
        let mut line = [0u8; 640];
        let length = encode_end(&mut line, 12, 0, 3);
        let text = std::str::from_utf8(&line[..length]).expect("ASCII");
        assert_eq!(text, "end\t12\t0\t3\n");
        assert_eq!(
            TraceEnd::parse(text.trim_end()),
            Some(TraceEnd {
                pid: 12,
                written: 0,
                failed: 3
            })
        );
        assert_eq!(TraceEnd::parse("end\t1\t2"), None, "a field short");
        assert_eq!(TraceEnd::parse("end\t1\t2\t3\t4"), None, "a field over");
        assert_eq!(
            TraceEnd::parse("end\t1\tx\t3"),
            None,
            "a count that is no number"
        );
        assert_eq!(TraceEnd::parse("get\t1\t2\t3"), None, "not an end line");
        let run = TracedRun::from_trace(
            "t",
            5,
            true,
            false,
            "get\t5\t5\t0\tTERM\nend\t5\t1\t0\nnot a line\n",
        );
        assert_eq!(
            (run.accesses.len(), run.ends.len(), run.unparsed),
            (1, 1, vec!["not a line".to_string()])
        );
    }

    #[test]
    fn a_putenv_string_names_what_precedes_its_first_equals() {
        assert_eq!(putenv_name(b"NAME=value=x"), b"NAME");
        assert_eq!(putenv_name(b"NAME"), b"NAME");
        assert_eq!(putenv_name(b"=x"), b"");
    }

    static LOOKUPS: AtomicUsize = AtomicUsize::new(0);

    fn finds_nothing(_name: &CStr) -> *mut c_void {
        LOOKUPS.fetch_add(1, Ordering::SeqCst);
        std::ptr::null_mut()
    }

    fn finds_something(_name: &CStr) -> *mut c_void {
        LOOKUPS.fetch_add(1, Ordering::SeqCst);
        std::ptr::NonNull::<c_void>::dangling().as_ptr()
    }

    fn fails_with_the_message(name: &CStr) -> ! {
        let mut message = [0u8; 256];
        let length = missing_message(&mut message, name.to_bytes());
        panic!("{}", String::from_utf8_lossy(&message[..length]))
    }

    /// The lookup the interposer forwards through, made to find nothing:
    /// it fails (in the binary, `missing` writes the message and aborts),
    /// never answers with a null pointer, and keeps nothing; one that finds
    /// a definition is looked up once and kept.
    #[test]
    fn a_lookup_that_finds_nothing_fails_loudly() {
        LOOKUPS.store(0, Ordering::SeqCst);
        let slot = AtomicPtr::new(std::ptr::null_mut());
        let caught = std::panic::catch_unwind(|| {
            real_with(
                &slot,
                c"no_such_symbol",
                finds_nothing,
                fails_with_the_message,
            )
        });
        let message = caught
            .expect_err("a lookup that finds nothing fails")
            .downcast::<String>()
            .map(|message| *message)
            .unwrap_or_default();
        assert_eq!(
            message,
            "env_trace: dlsym(RTLD_NEXT, no_such_symbol) found no definition, so the \
             interposer cannot forward; aborting\n"
        );
        assert!(slot.load(Ordering::SeqCst).is_null(), "nothing is kept");
        let found = real_with(&slot, c"getenv", finds_something, fails_with_the_message);
        let again = real_with(&slot, c"getenv", finds_nothing, fails_with_the_message);
        assert!(!found.is_null() && found == again, "the definition is kept");
        assert_eq!(LOOKUPS.load(Ordering::SeqCst), 2, "and looked up once");
        let mut buffer = [0u8; 256];
        let length = missing_message(&mut buffer, &[b'n'; 400]);
        assert_eq!(
            (length, buffer[255]),
            (256, b'\n'),
            "a long name is cut to the buffer"
        );
    }

    /// A write counts as whole only when it wrote every byte of the line; a
    /// short write and a failed one count as failed.
    #[test]
    fn a_short_or_failed_write_is_counted_as_failed() {
        let whole = AtomicU64::new(0);
        let failed = AtomicU64::new(0);
        tally(40, 40, &whole, &failed);
        tally(39, 40, &whole, &failed);
        tally(-1, 40, &whole, &failed);
        tally(0, 40, &whole, &failed);
        assert_eq!(
            (whole.load(Ordering::SeqCst), failed.load(Ordering::SeqCst)),
            (1, 3)
        );
    }

    /// A child that outlives its limit is stopped and said to be; one that
    /// ends first is not.
    #[test]
    fn a_child_past_its_time_limit_is_stopped() {
        let mut sleeper = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("start sleep");
        let started = std::time::Instant::now();
        let (status, stopped) = wait_or_stop(&mut sleeper, Duration::from_millis(200));
        assert!(stopped && !status.success(), "stopped at its limit");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "and long before it would have ended"
        );
        let mut quick = std::process::Command::new("true")
            .spawn()
            .expect("start true");
        let (status, stopped) = wait_or_stop(&mut quick, Duration::from_secs(30));
        assert!(
            !stopped && status.success(),
            "a child that ends first is not stopped"
        );
    }

    /// The driver runs what `cargo test` runs: an ignored test is left out,
    /// and so is the driver itself.
    #[test]
    fn the_driver_runs_neither_ignored_tests_nor_itself() {
        let listed: BTreeSet<String> = [
            "a",
            "ignored",
            "env_trace::every_environment_access_of_a_lib_test_holds_the_crate_lock",
        ]
        .iter()
        .map(|name| name.to_string())
        .collect();
        let ignored = BTreeSet::from(["ignored".to_string()]);
        assert_eq!(tests_to_run(&listed, &ignored), vec![&"a".to_string()]);
    }
}
