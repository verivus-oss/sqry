//! Test fixture for Rust import patterns
//!
//! Covers all 5 import patterns that the Rust plugin should handle:
//! 1. use foo::bar as baz (scoped with alias)
//! 2. use foo as bar (simple with alias)
//! 3. use foo::bar (scoped without alias)
//! 4. use foo (simple without alias)
//! 5. use foo::{bar, baz} (grouped imports)

// Pattern 1: Scoped identifier with alias
use std::collections::HashMap as Map;
use std::io::Result as IoResult;
use tokio::runtime::Runtime as TokioRuntime;

// Pattern 2: Simple identifier with alias
use std::io as StdIo;
use std::fs as FileSystem;

// Pattern 3: Scoped identifier without alias
use std::sync::Arc;
use std::collections::HashSet;
use tokio::sync::Mutex;

// Pattern 4: Simple identifier without alias
use tokio;
use serde;

// Pattern 5: Grouped imports (use lists)
use std::path::{Path, PathBuf};
use std::io::{Read, Write, BufRead};
use tokio::sync::{mpsc, oneshot};

// Additional edge cases
use std::sync::Arc as SyncArc;  // Scoped + alias combination
use std::{env, process};        // Grouped from root module

fn main() {
    // This function exists to make the file valid Rust
    println!("Import fixture file");
}
