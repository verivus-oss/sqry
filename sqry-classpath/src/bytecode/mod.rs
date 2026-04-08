//! JVM bytecode parsing.
//!
//! Parses `.class` files into `ClassStub` records, extracting:
//! - Class/method/field declarations with visibility and modifiers
//! - Generic type signatures (JVMS 4.7.9)
//! - Annotations (runtime visible and invisible)
//! - Lambda targets (`BootstrapMethods` attribute)
//! - Java 9+ module declarations
//!
//! ## JAR scanning
//!
//! The [`scan_jar`] function reads a JAR (ZIP archive), parses all `.class`
//! entries into enriched [`ClassStub`] records, and returns them. It applies
//! security limits to prevent JAR-bomb denial-of-service attacks.

// JVM bytecode values are spec-bounded; casts to u32 are intentional
#![allow(clippy::cast_possible_truncation)]

pub mod annotations;
pub mod classfile;
pub mod constants;
pub mod generics;
pub mod lambda;
pub mod modules;

use std::io::Read;
use std::path::Path;

use cafebabe::ParseOptions;
use cafebabe::attributes::AttributeData;
use log::warn;
use zip::ZipArchive;

use crate::stub::model::ClassStub;
use crate::{ClasspathError, ClasspathResult};

pub use classfile::parse_class;

// ---------------------------------------------------------------------------
// Security limits for JAR scanning
// ---------------------------------------------------------------------------

/// Maximum number of entries allowed in a single JAR file.
///
/// JARs with more entries are rejected to prevent ZIP-bomb denial-of-service
/// attacks and memory exhaustion from pathologically large archives.
const MAX_JAR_ENTRIES: usize = 100_000;

/// Maximum total uncompressed size allowed for a single JAR (2 GB).
///
/// If the sum of all entry sizes exceeds this limit, scanning is aborted.
const MAX_JAR_UNCOMPRESSED_SIZE: u64 = 2 * 1024 * 1024 * 1024;

// ---------------------------------------------------------------------------
// JAR scanning
// ---------------------------------------------------------------------------

/// Scan a JAR file (ZIP archive) and parse all `.class` entries into
/// [`ClassStub`] records.
///
/// Uses rayon for parallelism across JARs (called by the outer loop), but
/// processes entries within a single JAR sequentially since [`ZipArchive`] is
/// not `Send`.
///
/// # Security limits
///
/// - Entry count limit: 100,000 per JAR
/// - Uncompressed size limit: 2 GB per JAR
/// - Per-class errors are logged and skipped, never fail the whole JAR
///
/// # Errors
///
/// Returns [`ClasspathError::JarReadError`] if the JAR cannot be opened or
/// if it exceeds security limits.
pub fn scan_jar(jar_path: &Path) -> ClasspathResult<Vec<ClassStub>> {
    let jar_display = jar_path.display().to_string();

    let file = std::fs::File::open(jar_path).map_err(|e| ClasspathError::JarReadError {
        path: jar_display.clone(),
        reason: format!("cannot open file: {e}"),
    })?;

    let mut archive = ZipArchive::new(file).map_err(|e| ClasspathError::JarReadError {
        path: jar_display.clone(),
        reason: format!("invalid ZIP/JAR archive: {e}"),
    })?;

    // --- Security check: entry count ---
    let entry_count = archive.len();
    if entry_count > MAX_JAR_ENTRIES {
        return Err(ClasspathError::JarReadError {
            path: jar_display,
            reason: format!(
                "JAR bomb detected: {entry_count} entries exceeds limit of {MAX_JAR_ENTRIES}"
            ),
        });
    }

    // --- Security check: total uncompressed size ---
    let mut total_uncompressed: u64 = 0;
    for i in 0..entry_count {
        if let Ok(entry) = archive.by_index_raw(i) {
            total_uncompressed = total_uncompressed.saturating_add(entry.size());
        }
    }

    if total_uncompressed > MAX_JAR_UNCOMPRESSED_SIZE {
        return Err(ClasspathError::JarReadError {
            path: jar_display,
            reason: format!(
                "JAR bomb detected: total uncompressed size {total_uncompressed} bytes \
                 exceeds limit of {MAX_JAR_UNCOMPRESSED_SIZE} bytes (2 GB)"
            ),
        });
    }

    // --- Scan entries ---
    let mut stubs = Vec::new();
    for i in 0..entry_count {
        let mut entry = match archive.by_index(i) {
            Ok(e) => e,
            Err(e) => {
                warn!("JAR {jar_display}: cannot read entry {i}: {e}");
                continue;
            }
        };

        #[allow(clippy::case_sensitive_file_extension_comparisons)]
        // JVM classfiles always use .class extension
        let entry_name = entry.name().to_owned();

        // Only process .class files.
        #[allow(clippy::case_sensitive_file_extension_comparisons)]
        // Known file extensions in domain
        if !entry_name.ends_with(".class") {
            continue;
        }

        // Skip module-info and package-info — handled separately.
        if is_info_class(&entry_name) {
            continue;
        }

        // Read entry bytes.
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        if let Err(e) = entry.read_to_end(&mut bytes) {
            warn!("JAR {jar_display}: cannot read entry {entry_name}: {e}");
            continue;
        }

        // Parse and enrich.
        match parse_class_enriched(&bytes) {
            Ok(mut stub) => {
                stub.source_jar = Some(jar_display.clone());
                stubs.push(stub);
            }
            Err(e) => {
                warn!("JAR {jar_display}: cannot parse class {entry_name}: {e}");
            }
        }
    }

    Ok(stubs)
}

/// Parse a `.class` file and apply all enrichment passes in a single
/// cafebabe parse.
///
/// This combines base class parsing ([`parse_class`]) with generic signature
/// extraction, annotation extraction, lambda target extraction, and module
/// extraction — all from a single `cafebabe::ClassFile` parse.
///
/// Per-enrichment errors are logged and skipped; the base stub is always
/// returned if the initial parse succeeds.
fn parse_class_enriched(bytes: &[u8]) -> ClasspathResult<ClassStub> {
    // First parse with parse_class for the base stub.
    let mut stub = parse_class(bytes)?;

    // Second parse with cafebabe for enrichment data.
    // This is a lightweight re-parse since we disable bytecode parsing.
    let mut opts = ParseOptions::default();
    opts.parse_bytecode(false);

    let class_file = match cafebabe::parse_class_with_options(bytes, &opts) {
        Ok(cf) => cf,
        Err(e) => {
            warn!("enrichment parse failed for {}: {e}", stub.fqn);
            return Ok(stub);
        }
    };

    // --- Enrich: annotations (class-level) ---
    for attr in &class_file.attributes {
        match annotations::extract_annotations_from_attribute(&attr.data) {
            Ok(Some(ann)) => stub.annotations.extend(ann),
            Ok(None) => {}
            Err(e) => {
                warn!("annotation extraction failed for {}: {e}", stub.fqn);
            }
        }
    }

    // --- Enrich: annotations (method-level and field-level) ---
    enrich_method_annotations(&class_file, &mut stub);
    enrich_field_annotations(&class_file, &mut stub);

    // --- Enrich: generic signatures ---
    enrich_generics(&class_file, &mut stub);

    // --- Enrich: lambda targets ---
    stub.lambda_targets = lambda::extract_lambda_targets(&class_file);

    // --- Enrich: module info ---
    match modules::extract_module(&class_file) {
        Ok(Some(module)) => stub.module = Some(module),
        Ok(None) => {}
        Err(e) => {
            warn!("module extraction failed for {}: {e}", stub.fqn);
        }
    }

    Ok(stub)
}

/// Enrich method stubs with annotations from the cafebabe parse.
fn enrich_method_annotations(class_file: &cafebabe::ClassFile<'_>, stub: &mut ClassStub) {
    for (i, method) in class_file.methods.iter().enumerate() {
        if i >= stub.methods.len() {
            break;
        }
        // Find the matching stub method. The classfile parser may skip synthetic/bridge
        // methods, so we match by name + descriptor.
        let Some(method_stub) = stub.methods.iter_mut().find(|ms| {
            ms.name == method.name.as_ref() && ms.descriptor == method.descriptor.to_string()
        }) else {
            continue;
        };

        for attr in &method.attributes {
            match annotations::extract_annotations_from_attribute(&attr.data) {
                Ok(Some(ann)) => method_stub.annotations.extend(ann),
                Ok(None) => {}
                Err(e) => {
                    warn!(
                        "method annotation extraction failed for {}#{}: {e}",
                        stub.fqn, method_stub.name
                    );
                }
            }
            match annotations::extract_parameter_annotations_from_attribute(&attr.data) {
                Ok(Some(param_ann)) => {
                    // Merge parameter annotations: extend or replace.
                    if method_stub.parameter_annotations.is_empty() {
                        method_stub.parameter_annotations = param_ann;
                    } else {
                        for (pi, anns) in param_ann.into_iter().enumerate() {
                            if pi < method_stub.parameter_annotations.len() {
                                method_stub.parameter_annotations[pi].extend(anns);
                            } else {
                                method_stub.parameter_annotations.push(anns);
                            }
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    warn!(
                        "parameter annotation extraction failed for {}#{}: {e}",
                        stub.fqn, method_stub.name
                    );
                }
            }
        }
    }
}

/// Enrich field stubs with annotations from the cafebabe parse.
fn enrich_field_annotations(class_file: &cafebabe::ClassFile<'_>, stub: &mut ClassStub) {
    for field in &class_file.fields {
        let Some(field_stub) = stub
            .fields
            .iter_mut()
            .find(|fs| fs.name == field.name.as_ref())
        else {
            continue;
        };

        for attr in &field.attributes {
            match annotations::extract_annotations_from_attribute(&attr.data) {
                Ok(Some(ann)) => field_stub.annotations.extend(ann),
                Ok(None) => {}
                Err(e) => {
                    warn!(
                        "field annotation extraction failed for {}.{}: {e}",
                        stub.fqn, field_stub.name
                    );
                }
            }
        }
    }
}

/// Enrich stubs with generic type signatures from the cafebabe parse.
fn enrich_generics(class_file: &cafebabe::ClassFile<'_>, stub: &mut ClassStub) {
    // Class-level signature.
    for attr in &class_file.attributes {
        if let AttributeData::Signature(sig) = &attr.data {
            match generics::parse_class_signature(sig) {
                Ok(parsed) => stub.generic_signature = Some(parsed),
                Err(e) => {
                    warn!("class signature parse failed for {}: {e}", stub.fqn);
                }
            }
            break;
        }
    }

    // Method-level signatures.
    for method in &class_file.methods {
        let Some(method_stub) = stub.methods.iter_mut().find(|ms| {
            ms.name == method.name.as_ref() && ms.descriptor == method.descriptor.to_string()
        }) else {
            continue;
        };

        for attr in &method.attributes {
            if let AttributeData::Signature(sig) = &attr.data {
                match generics::parse_method_signature(sig) {
                    Ok(parsed) => method_stub.generic_signature = Some(parsed),
                    Err(e) => {
                        warn!(
                            "method signature parse failed for {}#{}: {e}",
                            stub.fqn, method_stub.name
                        );
                    }
                }
                break;
            }
        }
    }

    // Field-level signatures.
    for field in &class_file.fields {
        let Some(field_stub) = stub
            .fields
            .iter_mut()
            .find(|fs| fs.name == field.name.as_ref())
        else {
            continue;
        };

        for attr in &field.attributes {
            if let AttributeData::Signature(sig) = &attr.data {
                match generics::parse_field_signature(sig) {
                    Ok(parsed) => field_stub.generic_signature = Some(parsed),
                    Err(e) => {
                        warn!(
                            "field signature parse failed for {}.{}: {e}",
                            stub.fqn, field_stub.name
                        );
                    }
                }
                break;
            }
        }
    }
}

/// Check if a ZIP entry name is a `module-info.class` or `package-info.class`.
///
/// These are skipped during JAR scanning because they are handled by the
/// module enrichment parser (U07b) or are not useful for type resolution.
fn is_info_class(entry_name: &str) -> bool {
    let file_name = entry_name.rsplit('/').next().unwrap_or(entry_name);
    file_name == "module-info.class" || file_name == "package-info.class"
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    /// Build a minimal valid .class file for testing. This is a stripped-down
    /// version of the `ClassFileBuilder` from `classfile.rs` — just enough
    /// to produce parseable bytes.
    fn build_minimal_class(class_name: &str) -> Vec<u8> {
        let mut bytes = Vec::new();

        // Magic
        bytes.extend_from_slice(&0xCAFE_BABEu32.to_be_bytes());
        // Minor version
        bytes.extend_from_slice(&0u16.to_be_bytes());
        // Major version (52 = Java 8)
        bytes.extend_from_slice(&52u16.to_be_bytes());

        // Constant pool: 5 entries
        // #1: Utf8 <class_name>
        // #2: Class -> #1
        // #3: Utf8 "java/lang/Object"
        // #4: Class -> #3
        let class_bytes = class_name.as_bytes();
        let object_bytes = b"java/lang/Object";

        let cp_count: u16 = 5; // 4 entries + 1
        bytes.extend_from_slice(&cp_count.to_be_bytes());

        // #1: CONSTANT_Utf8 <class_name>
        bytes.push(1);
        bytes.extend_from_slice(&(class_bytes.len() as u16).to_be_bytes());
        bytes.extend_from_slice(class_bytes);

        // #2: CONSTANT_Class -> #1
        bytes.push(7);
        bytes.extend_from_slice(&1u16.to_be_bytes());

        // #3: CONSTANT_Utf8 "java/lang/Object"
        bytes.push(1);
        bytes.extend_from_slice(&(object_bytes.len() as u16).to_be_bytes());
        bytes.extend_from_slice(object_bytes);

        // #4: CONSTANT_Class -> #3
        bytes.push(7);
        bytes.extend_from_slice(&3u16.to_be_bytes());

        // Access flags: ACC_PUBLIC | ACC_SUPER
        bytes.extend_from_slice(&0x0021u16.to_be_bytes());
        // This class: #2
        bytes.extend_from_slice(&2u16.to_be_bytes());
        // Super class: #4
        bytes.extend_from_slice(&4u16.to_be_bytes());
        // Interfaces count: 0
        bytes.extend_from_slice(&0u16.to_be_bytes());
        // Fields count: 0
        bytes.extend_from_slice(&0u16.to_be_bytes());
        // Methods count: 0
        bytes.extend_from_slice(&0u16.to_be_bytes());
        // Attributes count: 0
        bytes.extend_from_slice(&0u16.to_be_bytes());

        bytes
    }

    /// Create an in-memory JAR (ZIP) file with the given entries.
    fn build_test_jar(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            for (name, data) in entries {
                writer.start_file(*name, options).unwrap();
                writer.write_all(data).unwrap();
            }
            writer.finish().unwrap();
        }
        buf
    }

    #[test]
    fn test_scan_jar_multiple_classes() {
        let class_a = build_minimal_class("com/example/ClassA");
        let class_b = build_minimal_class("com/example/ClassB");

        let jar_bytes = build_test_jar(&[
            ("com/example/ClassA.class", &class_a),
            ("com/example/ClassB.class", &class_b),
        ]);

        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), &jar_bytes).unwrap();

        let stubs = scan_jar(tmp.path()).unwrap();
        assert_eq!(stubs.len(), 2);

        let fqns: Vec<&str> = stubs.iter().map(|s| s.fqn.as_str()).collect();
        assert!(fqns.contains(&"com.example.ClassA"));
        assert!(fqns.contains(&"com.example.ClassB"));
    }

    #[test]
    fn test_scan_jar_empty() {
        let jar_bytes = build_test_jar(&[]);

        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), &jar_bytes).unwrap();

        let stubs = scan_jar(tmp.path()).unwrap();
        assert!(stubs.is_empty());
    }

    #[test]
    fn test_scan_jar_malformed_jar() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"this is not a zip file").unwrap();

        let result = scan_jar(tmp.path());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, ClasspathError::JarReadError { .. }),
            "expected JarReadError, got: {err}"
        );
    }

    #[test]
    fn test_scan_jar_skips_module_and_package_info() {
        let class_a = build_minimal_class("com/example/ClassA");
        // module-info and package-info would fail parse_class anyway,
        // but scan_jar should skip them before attempting parse.
        let jar_bytes = build_test_jar(&[
            ("com/example/ClassA.class", &class_a),
            ("module-info.class", b"not a real class"),
            ("com/example/package-info.class", b"not a real class"),
            // Multi-release variant
            ("META-INF/versions/11/module-info.class", b"not real"),
        ]);

        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), &jar_bytes).unwrap();

        let stubs = scan_jar(tmp.path()).unwrap();
        assert_eq!(stubs.len(), 1);
        assert_eq!(stubs[0].fqn, "com.example.ClassA");
    }

    #[test]
    fn test_scan_jar_inner_classes_included() {
        let outer = build_minimal_class("com/example/Outer");
        let inner = build_minimal_class("com/example/Outer$Inner");

        let jar_bytes = build_test_jar(&[
            ("com/example/Outer.class", &outer),
            ("com/example/Outer$Inner.class", &inner),
        ]);

        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), &jar_bytes).unwrap();

        let stubs = scan_jar(tmp.path()).unwrap();
        assert_eq!(stubs.len(), 2);

        let fqns: Vec<&str> = stubs.iter().map(|s| s.fqn.as_str()).collect();
        assert!(fqns.contains(&"com.example.Outer"));
        assert!(fqns.contains(&"com.example.Outer$Inner"));
    }

    #[test]
    fn test_scan_jar_skips_non_class_files() {
        let class_a = build_minimal_class("com/example/ClassA");

        let jar_bytes = build_test_jar(&[
            ("com/example/ClassA.class", &class_a),
            ("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\n"),
            ("com/example/resource.txt", b"some resource"),
        ]);

        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), &jar_bytes).unwrap();

        let stubs = scan_jar(tmp.path()).unwrap();
        assert_eq!(stubs.len(), 1);
        assert_eq!(stubs[0].fqn, "com.example.ClassA");
    }

    #[test]
    fn test_scan_jar_malformed_class_skipped() {
        let good_class = build_minimal_class("com/example/Good");

        let jar_bytes = build_test_jar(&[
            ("com/example/Good.class", &good_class),
            ("com/example/Bad.class", b"not valid bytecode"),
        ]);

        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), &jar_bytes).unwrap();

        let stubs = scan_jar(tmp.path()).unwrap();
        assert_eq!(stubs.len(), 1);
        assert_eq!(stubs[0].fqn, "com.example.Good");
    }

    #[test]
    fn test_scan_jar_nonexistent_file() {
        let result = scan_jar(Path::new("/nonexistent/path/foo.jar"));
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ClasspathError::JarReadError { .. }
        ));
    }

    #[test]
    fn test_is_info_class() {
        assert!(is_info_class("module-info.class"));
        assert!(is_info_class("com/example/package-info.class"));
        assert!(is_info_class("META-INF/versions/11/module-info.class"));
        assert!(!is_info_class("com/example/MyClass.class"));
        assert!(!is_info_class("com/example/ModuleInfo.class"));
    }
}
