//! Merged classpath index for fast FQN lookup.
//!
//! Combines all per-JAR stubs into a single sorted index with secondary
//! indices for package and annotation lookup. Persisted at
//! `.sqry/classpath/index.sqry` using postcard binary serialization.
//!
//! ## Lookup performance
//!
//! - **FQN lookup**: O(log n) via binary search on the sorted `classes` vec.
//! - **Package lookup**: O(1) via `package_index` (returns a slice range).
//! - **Annotation lookup**: O(1) via `annotation_index` (returns index list).
//!
//! ## String table
//!
//! The string table is reserved for future deduplication optimizations within
//! the persisted index. Currently it is populated but not used for indirection
//! — class stubs contain their own string data.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::stub::model::ClassStub;
use crate::{ClasspathError, ClasspathResult};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Current format version for the classpath index.
///
/// Increment this when the serialized format changes in a way that is not
/// backward-compatible. On load, a version mismatch triggers a full rebuild.
pub const CLASSPATH_INDEX_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// ClasspathIndex
// ---------------------------------------------------------------------------

/// Merged classpath index for fast FQN lookup.
///
/// Combines all per-JAR stubs into a single sorted index with secondary
/// indices for package and annotation lookup.
///
/// Persisted at: `.sqry/classpath/index.sqry`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClasspathIndex {
    /// Format version (independent from graph snapshot version).
    pub version: u32,
    /// Local string table for deduplication within the index.
    ///
    /// Reserved for future optimisation; currently populated with unique
    /// FQN strings but not used for indirection.
    pub string_table: Vec<String>,
    /// Classes sorted by FQN for binary search.
    pub classes: Vec<ClassStub>,
    /// Package index: package name to (start, end) range of indices into
    /// `classes`. The range is half-open: `classes[start..end]`.
    pub package_index: HashMap<String, (usize, usize)>,
    /// Annotation index: annotation FQN to list of class indices in
    /// `classes` that carry that annotation.
    pub annotation_index: HashMap<String, Vec<usize>>,
}

impl ClasspathIndex {
    /// Build an index from collected stubs.
    ///
    /// 1. Sorts classes by FQN.
    /// 2. Builds the package index (groups consecutive classes by package prefix).
    /// 3. Builds the annotation index (scans all class-level annotations).
    /// 4. Populates the string table with unique FQNs.
    #[must_use]
    pub fn build(mut stubs: Vec<ClassStub>) -> Self {
        // Sort by FQN for binary search.
        stubs.sort_by(|a, b| a.fqn.cmp(&b.fqn));

        // Build package index.
        let package_index = build_package_index(&stubs);

        // Build annotation index.
        let annotation_index = build_annotation_index(&stubs);

        // Build string table (unique FQNs, sorted).
        let mut string_table: Vec<String> = stubs.iter().map(|s| s.fqn.clone()).collect();
        string_table.dedup();

        Self {
            version: CLASSPATH_INDEX_VERSION,
            string_table,
            classes: stubs,
            package_index,
            annotation_index,
        }
    }

    /// Look up a class by fully qualified name (binary search).
    ///
    /// Returns `None` if no class with the given FQN exists in the index.
    #[must_use]
    pub fn lookup_fqn(&self, fqn: &str) -> Option<&ClassStub> {
        let idx = self
            .classes
            .binary_search_by_key(&fqn, |s| s.fqn.as_str())
            .ok()?;
        Some(&self.classes[idx])
    }

    /// Look up all classes in a package.
    ///
    /// Returns an empty slice if the package is not found.
    #[must_use]
    pub fn lookup_package(&self, package: &str) -> &[ClassStub] {
        match self.package_index.get(package) {
            Some(&(start, end)) => &self.classes[start..end],
            None => &[],
        }
    }

    /// Look up classes with a specific annotation.
    ///
    /// Returns an empty vec if no classes carry the given annotation.
    #[must_use]
    pub fn lookup_annotated(&self, annotation_fqn: &str) -> Vec<&ClassStub> {
        match self.annotation_index.get(annotation_fqn) {
            Some(indices) => indices
                .iter()
                .filter_map(|&i| self.classes.get(i))
                .collect(),
            None => vec![],
        }
    }

    /// Persist the index to disk at the given path.
    ///
    /// Uses atomic write (temp file + rename) to prevent corrupt reads.
    ///
    /// # Errors
    ///
    /// Returns [`ClasspathError::IndexError`] on serialization or I/O failure.
    pub fn save(&self, path: &Path) -> ClasspathResult<()> {
        // Ensure parent directory exists.
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                ClasspathError::IndexError(format!(
                    "cannot create index directory {}: {e}",
                    parent.display()
                ))
            })?;
        }

        let bytes = postcard::to_allocvec(self).map_err(|e| {
            ClasspathError::IndexError(format!("cannot serialize classpath index: {e}"))
        })?;

        // Atomic write: temp file + rename.
        let temp_path = path.with_extension("sqry.tmp");
        fs::write(&temp_path, &bytes).map_err(|e| {
            ClasspathError::IndexError(format!(
                "cannot write temp index file {}: {e}",
                temp_path.display()
            ))
        })?;

        fs::rename(&temp_path, path).map_err(|e| {
            // Clean up temp file on rename failure.
            let _ = fs::remove_file(&temp_path);
            ClasspathError::IndexError(format!(
                "cannot rename temp index to {}: {e}",
                path.display()
            ))
        })?;

        Ok(())
    }

    /// Load the index from disk.
    ///
    /// # Errors
    ///
    /// Returns [`ClasspathError::IndexError`] if:
    /// - The file cannot be read.
    /// - The file cannot be deserialized.
    /// - The format version does not match [`CLASSPATH_INDEX_VERSION`].
    pub fn load(path: &Path) -> ClasspathResult<Self> {
        let bytes = fs::read(path).map_err(|e| {
            ClasspathError::IndexError(format!(
                "cannot read classpath index {}: {e}",
                path.display()
            ))
        })?;

        let index: Self = postcard::from_bytes(&bytes).map_err(|e| {
            ClasspathError::IndexError(format!(
                "cannot deserialize classpath index {}: {e}",
                path.display()
            ))
        })?;

        if index.version != CLASSPATH_INDEX_VERSION {
            return Err(ClasspathError::IndexError(format!(
                "classpath index version mismatch: expected {CLASSPATH_INDEX_VERSION}, found {}",
                index.version
            )));
        }

        Ok(index)
    }
}

// ---------------------------------------------------------------------------
// Index building helpers
// ---------------------------------------------------------------------------

/// Build the package index from sorted stubs.
///
/// Groups consecutive classes by their package prefix (everything before the
/// last `.` in the FQN). For each package, records the `(start, end)` range
/// into the sorted classes vec.
fn build_package_index(sorted_classes: &[ClassStub]) -> HashMap<String, (usize, usize)> {
    let mut index: HashMap<String, (usize, usize)> = HashMap::new();
    if sorted_classes.is_empty() {
        return index;
    }

    let mut current_package = package_of(&sorted_classes[0].fqn);
    let mut range_start = 0;

    for (i, stub) in sorted_classes.iter().enumerate().skip(1) {
        let pkg = package_of(&stub.fqn);
        if pkg != current_package {
            index.insert(current_package, (range_start, i));
            current_package = pkg;
            range_start = i;
        }
    }

    // Insert the last group.
    index.insert(current_package, (range_start, sorted_classes.len()));

    index
}

/// Build the annotation index from sorted stubs.
///
/// For each class-level annotation, records the class index.
fn build_annotation_index(sorted_classes: &[ClassStub]) -> HashMap<String, Vec<usize>> {
    let mut index: HashMap<String, Vec<usize>> = HashMap::new();

    for (i, stub) in sorted_classes.iter().enumerate() {
        for ann in &stub.annotations {
            index.entry(ann.type_fqn.clone()).or_default().push(i);
        }
    }

    index
}

/// Extract the package name from a fully qualified class name.
///
/// For `"java.util.HashMap"` returns `"java.util"`.
/// For `"HashMap"` (default package) returns `""`.
fn package_of(fqn: &str) -> String {
    match fqn.rfind('.') {
        Some(pos) => fqn[..pos].to_owned(),
        None => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stub::model::{AccessFlags, AnnotationStub, ClassKind};
    use tempfile::TempDir;

    /// Create a minimal `ClassStub` for testing.
    fn make_stub(fqn: &str) -> ClassStub {
        ClassStub {
            fqn: fqn.to_owned(),
            name: fqn.rsplit('.').next().unwrap_or(fqn).to_owned(),
            kind: ClassKind::Class,
            access: AccessFlags::new(0x0021),
            superclass: Some("java.lang.Object".to_owned()),
            interfaces: vec![],
            methods: vec![],
            fields: vec![],
            annotations: vec![],
            generic_signature: None,
            inner_classes: vec![],
            lambda_targets: vec![],
            module: None,
            record_components: vec![],
            enum_constants: vec![],
            source_file: None,
            source_jar: None,
            kotlin_metadata: None,
            scala_signature: None,
        }
    }

    /// Create a `ClassStub` with annotations for testing.
    fn make_annotated_stub(fqn: &str, annotation_fqns: &[&str]) -> ClassStub {
        let mut stub = make_stub(fqn);
        stub.annotations = annotation_fqns
            .iter()
            .map(|a| AnnotationStub {
                type_fqn: (*a).to_owned(),
                elements: vec![],
                is_runtime_visible: true,
            })
            .collect();
        stub
    }

    #[test]
    fn test_roundtrip_save_load() {
        let tmp = TempDir::new().unwrap();
        let index_path = tmp.path().join("classpath/index.sqry");

        let stubs = vec![
            make_stub("com.example.Bar"),
            make_stub("com.example.Foo"),
            make_stub("java.util.HashMap"),
        ];

        let index = ClasspathIndex::build(stubs);
        index.save(&index_path).unwrap();

        let loaded = ClasspathIndex::load(&index_path).unwrap();
        assert_eq!(loaded.version, CLASSPATH_INDEX_VERSION);
        assert_eq!(loaded.classes.len(), 3);
        // Should be sorted.
        assert_eq!(loaded.classes[0].fqn, "com.example.Bar");
        assert_eq!(loaded.classes[1].fqn, "com.example.Foo");
        assert_eq!(loaded.classes[2].fqn, "java.util.HashMap");
    }

    #[test]
    fn test_binary_search_by_fqn() {
        let stubs = vec![
            make_stub("com.example.Alpha"),
            make_stub("com.example.Beta"),
            make_stub("com.example.Gamma"),
            make_stub("java.util.List"),
        ];

        let index = ClasspathIndex::build(stubs);

        let found = index.lookup_fqn("com.example.Beta");
        assert!(found.is_some());
        assert_eq!(found.unwrap().fqn, "com.example.Beta");

        let found = index.lookup_fqn("java.util.List");
        assert!(found.is_some());
        assert_eq!(found.unwrap().fqn, "java.util.List");

        let not_found = index.lookup_fqn("com.example.DoesNotExist");
        assert!(not_found.is_none());
    }

    #[test]
    fn test_package_index_lookup() {
        let stubs = vec![
            make_stub("com.example.Alpha"),
            make_stub("com.example.Beta"),
            make_stub("java.util.HashMap"),
            make_stub("java.util.List"),
            make_stub("java.util.Map"),
        ];

        let index = ClasspathIndex::build(stubs);

        let com_example = index.lookup_package("com.example");
        assert_eq!(com_example.len(), 2);
        assert_eq!(com_example[0].fqn, "com.example.Alpha");
        assert_eq!(com_example[1].fqn, "com.example.Beta");

        let java_util = index.lookup_package("java.util");
        assert_eq!(java_util.len(), 3);

        let empty = index.lookup_package("org.nonexistent");
        assert!(empty.is_empty());
    }

    #[test]
    fn test_annotation_index_lookup() {
        let stubs = vec![
            make_annotated_stub(
                "com.example.MyController",
                &["org.springframework.stereotype.Controller"],
            ),
            make_annotated_stub(
                "com.example.MyService",
                &["org.springframework.stereotype.Service"],
            ),
            make_annotated_stub(
                "com.example.AnotherController",
                &[
                    "org.springframework.stereotype.Controller",
                    "org.springframework.web.bind.annotation.RestController",
                ],
            ),
        ];

        let index = ClasspathIndex::build(stubs);

        let controllers = index.lookup_annotated("org.springframework.stereotype.Controller");
        assert_eq!(controllers.len(), 2);
        // Sorted by FQN.
        assert_eq!(controllers[0].fqn, "com.example.AnotherController");
        assert_eq!(controllers[1].fqn, "com.example.MyController");

        let services = index.lookup_annotated("org.springframework.stereotype.Service");
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].fqn, "com.example.MyService");

        let none = index.lookup_annotated("javax.persistence.Entity");
        assert!(none.is_empty());
    }

    #[test]
    fn test_empty_index() {
        let stubs: Vec<ClassStub> = vec![];
        let index = ClasspathIndex::build(stubs);

        assert_eq!(index.classes.len(), 0);
        assert!(index.lookup_fqn("anything").is_none());
        assert!(index.lookup_package("anything").is_empty());
        assert!(index.lookup_annotated("anything").is_empty());
    }

    #[test]
    fn test_version_mismatch_on_load() {
        let tmp = TempDir::new().unwrap();
        let index_path = tmp.path().join("index.sqry");

        let mut index = ClasspathIndex::build(vec![make_stub("com.example.Foo")]);
        index.version = 999; // Wrong version.
        index.save(&index_path).unwrap();

        let result = ClasspathIndex::load(&index_path);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("version mismatch"),
            "expected version mismatch error, got: {err_msg}"
        );
    }

    #[test]
    fn test_large_index_sort_and_search() {
        let stubs: Vec<ClassStub> = (0..1500)
            .map(|i| make_stub(&format!("com.example.Class{i:04}")))
            .collect();

        let index = ClasspathIndex::build(stubs);
        assert_eq!(index.classes.len(), 1500);

        // Verify sorted order.
        for window in index.classes.windows(2) {
            assert!(
                window[0].fqn <= window[1].fqn,
                "sort violation: {} > {}",
                window[0].fqn,
                window[1].fqn
            );
        }

        // Binary search should find specific entries.
        assert!(index.lookup_fqn("com.example.Class0000").is_some());
        assert!(index.lookup_fqn("com.example.Class0750").is_some());
        assert!(index.lookup_fqn("com.example.Class1499").is_some());
        assert!(index.lookup_fqn("com.example.Class1500").is_none());
    }

    #[test]
    fn test_load_corrupt_file() {
        let tmp = TempDir::new().unwrap();
        let index_path = tmp.path().join("index.sqry");
        fs::write(&index_path, b"corrupt garbage data").unwrap();

        let result = ClasspathIndex::load(&index_path);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ClasspathError::IndexError(_)),);
    }

    #[test]
    fn test_load_nonexistent_file() {
        let result = ClasspathIndex::load(Path::new("/nonexistent/index.sqry"));
        assert!(result.is_err());
    }

    #[test]
    fn test_default_package() {
        let stubs = vec![make_stub("DefaultClass"), make_stub("AnotherDefault")];

        let index = ClasspathIndex::build(stubs);

        // Default package is empty string.
        let default_pkg = index.lookup_package("");
        assert_eq!(default_pkg.len(), 2);
    }

    #[test]
    fn test_package_of() {
        assert_eq!(package_of("java.util.HashMap"), "java.util");
        assert_eq!(package_of("HashMap"), "");
        assert_eq!(
            package_of("com.example.deep.nested.Class"),
            "com.example.deep.nested"
        );
    }
}
