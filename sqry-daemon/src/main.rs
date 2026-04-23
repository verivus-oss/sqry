//! `sqryd` binary entry point — Task 9 U10.
//!
//! All logic lives in [`sqry_daemon::entrypoint`].  This file is intentionally
//! minimal so that integration tests can drive the daemon lifecycle via
//! `entrypoint::run()` without going through `std::process::exit`.

use std::process::ExitCode;

fn main() -> ExitCode {
    sqry_daemon::entrypoint::main_impl()
}
