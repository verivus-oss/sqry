//! Expand cache for macro-generated symbol storage (4.5f cache).
//!
//! Provides persistent storage of macro expansion results in
//! `.sqry/expand-cache/<crate-hash>.json`. This avoids re-running
//! `cargo expand` on every index build by caching qualified symbol names
//! per file.
//!
//! # Cache Security
//!
//! All symbol names read from the cache are validated against a safe character
//! pattern `[a-zA-Z0-9_:<> ]`. Names containing control characters, shell
//! metacharacters, or HTML entities are rejected with a warning. This prevents
//! cache poisoning via crafted JSON files.
//!
//! # Cache Freshness
//!
//! Each cache entry stores a SHA-256 hash of the original source file. If the
//! source has changed since the cache was written, the entry is stale and
//! skipped with a warning.
//!
//! # Performance Guard
//!
//! Expansion output is capped at 10MB per file. Files exceeding this limit
//! are skipped with a warning and confidence limitation.

use std::io::{self, BufReader, BufWriter};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Maximum size of expansion output per file (10 MB).
const MAX_EXPANSION_SIZE: usize = 10 * 1024 * 1024;

/// On-disk schema version for the expand-cache JSON payload.
///
/// INDEPENDENT of the graph snapshot magic constant: this is a regenerable
/// sidecar under `.sqry/expand-cache/`, not graph state. A reader that finds a
/// mismatching `schema_version` treats the entry as a soft miss (skip); the user
/// re-runs `sqry cache expand`. Bumped 1 -> 2 by the Phase 1b payload redesign
/// (per-file collapsed simple-name diff replaced by a flat, qualified, kinded
/// `generated_symbols` list).
pub const EXPAND_CACHE_SCHEMA_VERSION: u32 = 2;

/// Pattern for validating symbol names read from cache.
/// Allows alphanumeric characters, underscores, colons (for qualified names),
/// angle brackets (for generics), spaces (for `impl Trait for Type`), and
/// ampersands/lifetimes (for `&'a`).
fn is_valid_symbol_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    name.chars().all(|c| {
        c.is_alphanumeric()
            || c == '_'
            || c == ':'
            || c == '<'
            || c == '>'
            || c == ' '
            || c == '&'
            || c == '\''
            || c == '.'
            || c == ','
    })
}

/// Declaration kind recovered from the expanded tree-sitter node.
///
/// Drives the `GraphBuildHelper` sink choice (and therefore the `NodeKind`) when
/// the Phase 1b index consumer materialises a generated symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeneratedSymbolKind {
    /// Free function (`fn` at module scope).
    Function,
    /// Method (`fn` inside an `impl` block).
    Method,
    /// Struct declaration.
    Struct,
    /// Enum declaration.
    Enum,
    /// Trait declaration.
    Trait,
    /// Type alias (`type X = ...`).
    TypeAlias,
    /// Constant or static.
    Constant,
    /// Module declaration.
    Module,
    /// Impl block (no direct node; fallback sink).
    Impl,
    /// Anything else recovered from the tree.
    Other,
}

/// One ordered scope segment on the path from the crate root down to a symbol.
///
/// Mirrors exactly what the live `scope_node_name` pushes: a segment for
/// `mod_item`, `struct_item`, `enum_item`, `trait_item`, `type_item` (and
/// nothing for `function_item` / `const_item` / `static_item` / `union_item` or
/// `impl_item`). `is_module` distinguishes the leading module run (the
/// crate-relative ownership axis) from the trailing type run (struct / enum /
/// trait / type wrappers), so the index consumer can split the chain at the
/// `file_module_path` boundary exactly as the live pipeline does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeSegment {
    /// Segment identifier (module name or type/trait name).
    pub name: String,
    /// True for `mod_item`; false for `struct_item` / `enum_item` /
    /// `trait_item` / `type_item`. Modules always precede types in a Rust scope
    /// chain (a type cannot contain a `mod`), so the leading `is_module` run is
    /// the owning module path.
    pub is_module: bool,
}

/// One macro-generated item, carried as STRUCTURED PIECES (not a pre-joined
/// name) so the index consumer can reconstruct a name that matches the live
/// graph's naming convention exactly by reusing the shared `build_qualified_name`.
///
/// It deliberately does NOT persist the extractor's crate-prefixed
/// `qualified_name`: the live Rust graph names symbols with no crate prefix and
/// with the plain impl type even for trait impls, so reusing `qualified_name`
/// verbatim would place nodes outside the graph namespace. The crate-prefixed
/// `qualified_name` remains purely a WRITE-TIME diff key and is discarded before
/// it reaches disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedSymbol {
    /// Simple identifier (e.g. "clone", "new"). No path, no crate.
    pub simple_name: String,
    /// The FULL ordered scope chain from the crate root to (but not including)
    /// this symbol, matching the segments `scope_node_name` pushes. The leading
    /// `is_module` run is the owning module path (comparable byte-for-byte with
    /// `file_module_path`); the trailing type run feeds `build_qualified_name`
    /// after the `file_module_path` prefix is stripped.
    pub scope_segments: Vec<ScopeSegment>,
    /// For methods: the plain impl-block type text, trimmed (e.g. "MyStruct").
    /// `None` for free items. This is the impl `type` field text ONLY, matching
    /// the live graph's `current_impl_type`, which drops the trait even for
    /// `impl Trait for Type`. NOT the `<Type as Trait>` decoration.
    pub impl_type: Option<String>,
    /// Declaration kind (drives the `NodeKind` / helper sink choice).
    pub kind: GeneratedSymbolKind,
}

/// Top-level cache entry for a single crate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpandCacheEntry {
    /// Schema discriminator; readers reject a mismatch as a soft miss (skip).
    pub schema_version: u32,
    /// Crate name.
    pub crate_name: String,
    /// Rust compiler version used for expansion.
    pub rust_version: String,
    /// ISO 8601 timestamp of when the cache was generated.
    pub generated_at: String,
    /// SHA-256 hash of the crate source (all `.rs` files concatenated).
    pub source_hash: String,
    /// Crate-wide expansion confidence (`"verified"` | `"heuristic"` |
    /// `"non_deterministic"`).
    pub confidence: String,
    /// Every macro-generated symbol in the crate, deduped at write time by the
    /// `(scope_segments, impl_type, simple_name)` tuple.
    pub generated_symbols: Vec<GeneratedSymbol>,
}

/// Expand cache manager.
///
/// Handles reading, writing, and freshness checking of the expand cache
/// directory at `.sqry/expand-cache/`.
#[derive(Debug)]
pub struct ExpandCache {
    /// Root directory of the expand cache (e.g., `.sqry/expand-cache/`).
    cache_dir: PathBuf,
}

impl ExpandCache {
    /// Create a new expand cache manager for the given directory.
    ///
    /// Creates the directory if it does not exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created.
    pub fn new(cache_dir: PathBuf) -> io::Result<Self> {
        std::fs::create_dir_all(&cache_dir)?;
        Ok(Self { cache_dir })
    }

    /// Read a cache entry for a crate.
    ///
    /// Returns `None` if the cache file does not exist. Returns an error if
    /// the file exists but cannot be parsed.
    ///
    /// # Security
    ///
    /// All symbol names in the returned entry are validated against the safe
    /// character pattern. Invalid names are stripped with a warning.
    ///
    /// # Errors
    ///
    /// Returns an error if the cache file exists but cannot be read or parsed.
    pub fn read(&self, crate_key: &str) -> io::Result<Option<ExpandCacheEntry>> {
        let path = self.cache_file_path(crate_key);
        if !path.exists() {
            return Ok(None);
        }

        // Security: check file size before deserializing to prevent OOM from
        // crafted oversized cache files.
        let metadata = std::fs::metadata(&path)?;
        if metadata.len() > MAX_EXPANSION_SIZE as u64 {
            log::warn!(
                "Expand cache file {} exceeds size limit ({} bytes > {} bytes), skipping",
                path.display(),
                metadata.len(),
                MAX_EXPANSION_SIZE,
            );
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Expand cache file exceeds size limit: {} bytes > {} bytes",
                    metadata.len(),
                    MAX_EXPANSION_SIZE,
                ),
            ));
        }

        let file = std::fs::File::open(&path)?;
        let reader = BufReader::new(file);
        let mut entry: ExpandCacheEntry = serde_json::from_reader(reader).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to parse expand cache {}: {e}", path.display()),
            )
        })?;

        // Validate all symbol names for security.
        sanitize_cache_entry(&mut entry);

        Ok(Some(entry))
    }

    /// Write a cache entry for a crate.
    ///
    /// Overwrites any existing cache file for this crate hash.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written.
    pub fn write(&self, crate_key: &str, entry: &ExpandCacheEntry) -> io::Result<()> {
        let path = self.cache_file_path(crate_key);
        let file = std::fs::File::create(&path)?;
        let writer = BufWriter::new(file);
        serde_json::to_writer_pretty(writer, entry).map_err(|e| {
            io::Error::other(format!(
                "Failed to write expand cache {}: {e}",
                path.display()
            ))
        })
    }

    /// Check if a cache entry is fresh (source hash matches).
    ///
    /// Returns `true` if the cache entry exists and its source hash matches
    /// the provided current hash. Returns `false` if the entry is stale or
    /// does not exist.
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` if reading the cache file fails.
    pub fn is_fresh(&self, crate_key: &str, current_source_hash: &str) -> io::Result<bool> {
        match self.read(crate_key)? {
            Some(entry) => Ok(entry.source_hash == current_source_hash),
            None => Ok(false),
        }
    }

    /// Remove a cache entry for a crate.
    ///
    /// No-op if the cache file does not exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be removed.
    pub fn remove(&self, crate_key: &str) -> io::Result<()> {
        let path = self.cache_file_path(crate_key);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// List all cached crate hashes.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be read.
    pub fn list_cached_crates(&self) -> io::Result<Vec<String>> {
        let mut crates = Vec::new();
        for entry in std::fs::read_dir(&self.cache_dir)? {
            let entry = entry?;
            if let Some(name) = entry.file_name().to_str()
                && let Some(hash) = name.strip_suffix(".json")
            {
                crates.push(hash.to_string());
            }
        }
        Ok(crates)
    }

    /// Get the file path for a cache entry.
    fn cache_file_path(&self, crate_key: &str) -> PathBuf {
        self.cache_dir.join(format!("{crate_key}.json"))
    }

    /// Returns the maximum expansion size per file.
    #[must_use]
    pub const fn max_expansion_size() -> usize {
        MAX_EXPANSION_SIZE
    }
}

/// Sanitize a cache entry by removing generated symbols carrying an invalid
/// name in any of their structured pieces (`simple_name`, `impl_type`, or any
/// `scope_segments[*].name`).
///
/// Logs a warning for each symbol removed. This prevents cache poisoning via
/// crafted JSON files (SEC-3): every string that could become part of a graph
/// node name is validated before the symbol is retained.
fn sanitize_cache_entry(entry: &mut ExpandCacheEntry) {
    let crate_name = entry.crate_name.clone();
    let before = entry.generated_symbols.len();

    entry
        .generated_symbols
        .retain(|sym| generated_symbol_is_valid(sym, &crate_name));

    let removed = before - entry.generated_symbols.len();
    if removed > 0 {
        log::warn!(
            "Removed {removed} invalid generated symbol(s) from expand cache for '{crate_name}' \
             (possible cache poisoning)"
        );
    }
}

/// Validate every name-bearing piece of a generated symbol.
fn generated_symbol_is_valid(sym: &GeneratedSymbol, crate_name: &str) -> bool {
    if !validate_and_warn(&sym.simple_name, crate_name) {
        return false;
    }
    if let Some(impl_type) = &sym.impl_type
        && !validate_and_warn(impl_type, crate_name)
    {
        return false;
    }
    for segment in &sym.scope_segments {
        if !validate_and_warn(&segment.name, crate_name) {
            return false;
        }
    }
    true
}

/// Validate a symbol name and log a warning if invalid.
fn validate_and_warn(name: &str, crate_name: &str) -> bool {
    if is_valid_symbol_name(name) {
        true
    } else {
        log::warn!(
            "Rejecting invalid symbol name '{name}' from expand cache for '{crate_name}' \
             (possible cache poisoning)"
        );
        false
    }
}

/// Compute a SHA-256 hash over every `.rs` file under `crate_root`, plus the
/// file count, for expand-cache freshness (`is_fresh`).
///
/// This is the SINGLE hash used on both sides of the cache contract: the
/// `sqry cache expand` writer and the index-side consumer both call it so their
/// `source_hash` values are byte-identical for the same tree. It is pure
/// filesystem work (`walkdir` + `sha2`, no subprocess), preserving the
/// execution-free index invariant.
///
/// # Errors
///
/// Returns an error if a discovered `.rs` file cannot be read.
pub fn compute_crate_source_hash(crate_root: &Path) -> io::Result<String> {
    use sha2::{Digest, Sha256};
    use walkdir::WalkDir;

    let mut hasher = Sha256::new();
    let mut file_count = 0u64;

    let mut paths: Vec<PathBuf> = WalkDir::new(crate_root)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|e| e.file_type().is_file() && e.path().extension().is_some_and(|ext| ext == "rs"))
        .map(walkdir::DirEntry::into_path)
        .collect();

    // Sort for deterministic hashing.
    paths.sort();

    for path in &paths {
        let content = std::fs::read(path)?;
        hasher.update(&content);
        file_count += 1;
    }

    // Include the file count so additions/deletions change the hash.
    hasher.update(file_count.to_le_bytes());

    Ok(hex::encode(hasher.finalize()))
}

/// Validate a symbol name for cache security.
///
/// Public interface for the validation function, useful for testing
/// and for other modules that need to validate symbol names before
/// inserting them into the cache.
#[must_use]
pub fn validate_symbol_name(name: &str) -> bool {
    is_valid_symbol_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_symbol(simple: &str) -> GeneratedSymbol {
        GeneratedSymbol {
            simple_name: simple.to_string(),
            scope_segments: vec![ScopeSegment {
                name: "MyStruct".to_string(),
                is_module: false,
            }],
            impl_type: Some("MyStruct".to_string()),
            kind: GeneratedSymbolKind::Method,
        }
    }

    fn sample_entry(source_hash: &str, symbols: Vec<GeneratedSymbol>) -> ExpandCacheEntry {
        ExpandCacheEntry {
            schema_version: EXPAND_CACHE_SCHEMA_VERSION,
            crate_name: "my_crate".to_string(),
            rust_version: "1.94.0".to_string(),
            generated_at: "2026-03-30T00:00:00Z".to_string(),
            source_hash: source_hash.to_string(),
            confidence: "heuristic".to_string(),
            generated_symbols: symbols,
        }
    }

    #[test]
    fn test_cache_read_write() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let cache = ExpandCache::new(temp_dir.path().join("expand-cache")).unwrap();

        let entry = sample_entry("abc123", vec![sample_symbol("clone")]);

        cache.write("my_crate", &entry).unwrap();
        let read_back = cache.read("my_crate").unwrap().unwrap();

        assert_eq!(read_back.schema_version, EXPAND_CACHE_SCHEMA_VERSION);
        assert_eq!(read_back.crate_name, "my_crate");
        assert_eq!(read_back.source_hash, "abc123");
        assert_eq!(read_back.confidence, "heuristic");
        assert_eq!(read_back.generated_symbols.len(), 1);
        let sym = &read_back.generated_symbols[0];
        assert_eq!(sym.simple_name, "clone");
        assert_eq!(sym.impl_type.as_deref(), Some("MyStruct"));
        assert_eq!(sym.kind, GeneratedSymbolKind::Method);
        assert_eq!(sym.scope_segments.len(), 1);
        assert_eq!(sym.scope_segments[0].name, "MyStruct");
        assert!(!sym.scope_segments[0].is_module);
    }

    #[test]
    fn test_cache_freshness_check() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let cache = ExpandCache::new(temp_dir.path().join("expand-cache")).unwrap();

        let entry = sample_entry("hash_v1", vec![]);
        cache.write("test_crate", &entry).unwrap();

        // Same hash → fresh.
        assert!(cache.is_fresh("test_crate", "hash_v1").unwrap());
        // Different hash → stale.
        assert!(!cache.is_fresh("test_crate", "hash_v2").unwrap());
        // Non-existent → not fresh.
        assert!(!cache.is_fresh("nonexistent", "hash_v1").unwrap());
    }

    #[test]
    fn test_validate_symbol_name() {
        assert!(validate_symbol_name("my_crate::MyStruct"));
        assert!(validate_symbol_name("my_crate::<MyStruct as Debug>::fmt"));
        assert!(validate_symbol_name("simple_name"));
        assert!(validate_symbol_name("a"));

        // Invalid names.
        assert!(!validate_symbol_name(""));
        assert!(!validate_symbol_name("name\x00with_null"));
        assert!(!validate_symbol_name("name;drop table"));
        assert!(!validate_symbol_name("name$(shell)"));
        assert!(!validate_symbol_name("name`cmd`"));
    }

    #[test]
    fn test_sanitize_cache_entry_removes_invalid() {
        let good = GeneratedSymbol {
            simple_name: "valid_name".to_string(),
            scope_segments: vec![ScopeSegment {
                name: "module".to_string(),
                is_module: true,
            }],
            impl_type: None,
            kind: GeneratedSymbolKind::Function,
        };
        // Bad simple_name.
        let bad_simple = GeneratedSymbol {
            simple_name: "exploit$(cmd)".to_string(),
            scope_segments: vec![],
            impl_type: None,
            kind: GeneratedSymbolKind::Function,
        };
        // Bad scope segment.
        let bad_scope = GeneratedSymbol {
            simple_name: "ok".to_string(),
            scope_segments: vec![ScopeSegment {
                name: "seg;drop".to_string(),
                is_module: true,
            }],
            impl_type: None,
            kind: GeneratedSymbolKind::Function,
        };
        // Bad impl_type.
        let bad_impl = GeneratedSymbol {
            simple_name: "ok".to_string(),
            scope_segments: vec![],
            impl_type: Some("Type`cmd`".to_string()),
            kind: GeneratedSymbolKind::Method,
        };

        let mut entry = sample_entry("abc", vec![good.clone(), bad_simple, bad_scope, bad_impl]);
        sanitize_cache_entry(&mut entry);

        assert_eq!(entry.generated_symbols.len(), 1);
        assert_eq!(entry.generated_symbols[0], good);
    }

    #[test]
    fn test_wrong_schema_version_round_trips_field() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let cache = ExpandCache::new(temp_dir.path().join("expand-cache")).unwrap();

        let mut entry = sample_entry("abc", vec![]);
        entry.schema_version = 1; // simulate an old v1 cache
        cache.write("legacy", &entry).unwrap();

        // `read` still deserializes; the version-mismatch soft miss is enforced
        // by the consumer, which inspects `schema_version`.
        let read_back = cache.read("legacy").unwrap().unwrap();
        assert_eq!(read_back.schema_version, 1);
        assert_ne!(read_back.schema_version, EXPAND_CACHE_SCHEMA_VERSION);
    }

    #[test]
    fn test_cache_remove() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let cache = ExpandCache::new(temp_dir.path().join("expand-cache")).unwrap();

        let entry = sample_entry("abc", vec![]);
        cache.write("removable", &entry).unwrap();
        assert!(cache.read("removable").unwrap().is_some());

        cache.remove("removable").unwrap();
        assert!(cache.read("removable").unwrap().is_none());
    }

    #[test]
    fn test_cache_list_crates() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let cache = ExpandCache::new(temp_dir.path().join("expand-cache")).unwrap();

        let entry = sample_entry("abc", vec![]);
        cache.write("crate_a", &entry).unwrap();
        cache.write("crate_b", &entry).unwrap();

        let mut crates = cache.list_cached_crates().unwrap();
        crates.sort();
        assert_eq!(crates, vec!["crate_a", "crate_b"]);
    }

    #[test]
    fn test_max_expansion_size() {
        assert_eq!(ExpandCache::max_expansion_size(), 10 * 1024 * 1024);
    }

    #[test]
    fn test_compute_crate_source_hash_stable_and_sensitive() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let root = temp_dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), b"fn a() {}\n").unwrap();

        let h1 = compute_crate_source_hash(root).unwrap();
        let h2 = compute_crate_source_hash(root).unwrap();
        assert_eq!(h1, h2, "hash must be stable for identical trees");

        // Mutating a source file changes the hash.
        std::fs::write(root.join("src/lib.rs"), b"fn a() { let _ = 1; }\n").unwrap();
        let h3 = compute_crate_source_hash(root).unwrap();
        assert_ne!(h1, h3, "hash must change when source changes");

        // Adding a file changes the hash (file-count is folded in).
        std::fs::write(root.join("src/extra.rs"), b"fn b() {}\n").unwrap();
        let h4 = compute_crate_source_hash(root).unwrap();
        assert_ne!(h3, h4, "hash must change when a file is added");
    }
}
