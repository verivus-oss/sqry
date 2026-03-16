//! Shell completions generation

use crate::args::{Cli, Shell, headings};
use anyhow::Result;
use clap_complete::{generate, shells};
use std::io;

/// Generate shell completions and output to stdout.
///
/// # Errors
/// Returns an error if writing to stdout fails.
#[allow(clippy::unnecessary_wraps)] // Keep Result for uniform command handling.
pub fn run_completions(shell: Shell) -> Result<()> {
    // Use taxonomy-aware command and normalize for deterministic output
    let mut cmd = headings::normalize(Cli::command_with_taxonomy());
    let bin_name = "sqry";

    match shell {
        Shell::Bash => {
            generate(shells::Bash, &mut cmd, bin_name, &mut io::stdout());
        }
        Shell::Zsh => {
            generate(shells::Zsh, &mut cmd, bin_name, &mut io::stdout());
        }
        Shell::Fish => {
            generate(shells::Fish, &mut cmd, bin_name, &mut io::stdout());
        }
        Shell::PowerShell => {
            generate(shells::PowerShell, &mut cmd, bin_name, &mut io::stdout());
        }
        Shell::Elvish => {
            generate(shells::Elvish, &mut cmd, bin_name, &mut io::stdout());
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::large_stack_test;

    large_stack_test! {
        #[test]
        fn test_completions_generation() {
            // Test that completions generation doesn't panic
            // We can't easily test the output, but we can verify it runs
            let shells = [
                Shell::Bash,
                Shell::Zsh,
                Shell::Fish,
                Shell::PowerShell,
                Shell::Elvish,
            ];

            for shell in shells {
                // This would output to stdout, so we just check it doesn't panic
                // In a real test environment, we'd capture stdout
                assert!(run_completions(shell).is_ok());
            }
        }
    }
}
