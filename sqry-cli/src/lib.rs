//! sqry-cli library exports
//!
//! This module exists to make certain functionality available to integration tests
//! and benchmarks. The main binary is still in `main.rs`.

pub mod args;
pub mod commands;
pub mod error;
pub mod index_discovery;
pub mod output;
pub mod persistence;
pub mod plugin_defaults;
pub mod progress;

/// Wrap a `#[test]` body in a 16 MB-stack thread.
///
/// Clap's deep subcommand tree causes `Cli::parse_from` to overflow the
/// default 8 MB debug-mode thread stack via deep recursion (not enum size).
/// Use this macro around any test that constructs a `Cli` value.
///
/// ```ignore
/// large_stack_test! {
///     #[test]
///     fn my_test() {
///         let cli = Cli::parse_from(["sqry", "index"]);
///         assert!(cli.command.is_some());
///     }
/// }
/// ```
#[cfg(test)]
macro_rules! large_stack_test {
    ($(#[$attr:meta])* fn $name:ident() $body:block) => {
        $(#[$attr])*
        fn $name() {
            let result = std::thread::Builder::new()
                .stack_size(16 * 1024 * 1024)
                .spawn(move || $body)
                .expect("spawn test thread")
                .join();
            if let Err(panic) = result {
                std::panic::resume_unwind(panic);
            }
        }
    };
}
#[cfg(test)]
pub(crate) use large_stack_test;
