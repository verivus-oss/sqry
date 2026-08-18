//! End-to-end integration tests for the JVM classpath pipeline.
//!
//! Tests cover:
//! 1. Build system detection from fixture projects
//! 2. Pipeline execution with manual classpath files (no subprocess)
//! 3. Index persistence roundtrip (save → load → verify identical)
//! 4. FQN workspace shadowing (workspace FQN takes precedence)
//! 5. Package and annotation index lookup
//! 6. Full pipeline cache-fallback tests for build-system-resolved classpaths

use std::io::Write;
use std::path::{Path, PathBuf};

use sqry_classpath::detect::{BuildSystem, detect_build_system};
use sqry_classpath::pipeline::{ClasspathConfig, ClasspathDepth, run_classpath_pipeline};
use sqry_classpath::resolve::{ClasspathEntry, ResolvedClasspath};
use sqry_classpath::stub::index::ClasspathIndex;
use sqry_classpath::stub::model::{
    AccessFlags, AnnotationStub, BaseType, ClassKind, ClassStub, FieldStub, MethodStub,
    TypeSignature,
};
use tempfile::TempDir;
use zip::write::SimpleFileOptions;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Path to the test-fixtures/jvm-classpath directory relative to the crate root.
fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-fixtures/jvm-classpath")
}

/// Build a minimal valid `.class` file for testing.
///
/// Creates a class that extends `java.lang.Object` with no methods, fields, or
/// attributes. Good enough for `scan_jar` to parse and produce a `ClassStub`.
fn build_minimal_class(class_name: &str) -> Vec<u8> {
    let mut bytes = Vec::new();

    // Magic number.
    bytes.extend_from_slice(&0xCAFE_BABEu32.to_be_bytes());
    // Minor version.
    bytes.extend_from_slice(&0u16.to_be_bytes());
    // Major version (52 = Java 8).
    bytes.extend_from_slice(&52u16.to_be_bytes());

    // Constant pool: 5 entries (index 1-4, pool count = 5).
    let class_bytes = class_name.as_bytes();
    let object_bytes = b"java/lang/Object";

    let cp_count: u16 = 5;
    bytes.extend_from_slice(&cp_count.to_be_bytes());

    // #1: CONSTANT_Utf8 <class_name>
    bytes.push(1);
    #[allow(clippy::cast_possible_truncation)] // Test assertion values are small constants
    bytes.extend_from_slice(&(class_bytes.len() as u16).to_be_bytes());
    bytes.extend_from_slice(class_bytes);

    // #2: CONSTANT_Class -> #1
    bytes.push(7);
    bytes.extend_from_slice(&1u16.to_be_bytes());

    // #3: CONSTANT_Utf8 "java/lang/Object"
    bytes.push(1);
    #[allow(clippy::cast_possible_truncation)] // Test assertion values are small constants
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

/// Create an in-memory JAR (ZIP archive) containing the specified `.class` entries.
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

/// Write a test JAR file to disk and return its path.
fn write_test_jar(dir: &Path, name: &str, classes: &[(&str, &[u8])]) -> PathBuf {
    let jar_bytes = build_test_jar(classes);
    let jar_path = dir.join(name);
    std::fs::write(&jar_path, &jar_bytes).unwrap();
    jar_path
}

/// Create a minimal `ClassStub` for testing (no methods, fields, or annotations).
fn make_stub(fqn: &str) -> ClassStub {
    ClassStub {
        fqn: fqn.to_owned(),
        name: fqn.rsplit('.').next().unwrap_or(fqn).to_owned(),
        kind: ClassKind::Class,
        access: AccessFlags::new(0x0021), // ACC_PUBLIC | ACC_SUPER
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

/// Create a `ClassStub` with methods for testing workspace shadowing.
fn make_stub_with_methods(fqn: &str, method_names: &[&str]) -> ClassStub {
    let mut stub = make_stub(fqn);
    stub.methods = method_names
        .iter()
        .map(|name| MethodStub {
            name: (*name).to_owned(),
            descriptor: "()V".to_owned(),
            access: AccessFlags::new(0x0001), // ACC_PUBLIC
            annotations: vec![],
            parameter_annotations: vec![],
            generic_signature: None,
            parameter_names: vec![],
            return_type: TypeSignature::Base(BaseType::Void),
            parameter_types: vec![],
        })
        .collect();
    stub
}

/// Create a `ClassStub` with annotations for testing annotation index lookup.
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

/// Create a `ClassStub` with fields for testing field-level resolution.
fn make_stub_with_fields(fqn: &str, field_names: &[(&str, &str)]) -> ClassStub {
    let mut stub = make_stub(fqn);
    stub.fields = field_names
        .iter()
        .map(|(name, descriptor)| FieldStub {
            name: (*name).to_owned(),
            descriptor: (*descriptor).to_owned(),
            access: AccessFlags::new(0x0001), // ACC_PUBLIC
            annotations: vec![],
            generic_signature: None,
            constant_value: None,
        })
        .collect();
    stub
}

/// Write a manual classpath file listing the given JAR paths, one per line.
fn write_classpath_file(dir: &Path, jar_paths: &[&Path]) -> PathBuf {
    let cp_file = dir.join("classpath.txt");
    let contents: String = jar_paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&cp_file, format!("{contents}\n")).unwrap();
    cp_file
}

fn copy_fixture_to_temp(fixture_name: &str) -> TempDir {
    let source = fixtures_dir().join(fixture_name);
    assert!(source.exists(), "fixture not found: {}", source.display());
    let tmp = TempDir::new().unwrap();
    copy_dir_recursive(&source, tmp.path());
    tmp
}

fn copy_dir_recursive(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if source_path.is_dir() {
            copy_dir_recursive(&source_path, &destination_path);
        } else {
            std::fs::copy(&source_path, &destination_path).unwrap();
        }
    }
}

fn write_cached_resolved_classpath(project_root: &Path, cache_file_name: &str) {
    let jar_path = write_test_jar(
        project_root,
        "cached-dependency.jar",
        &[(
            "com/example/CachedDependency.class",
            &build_minimal_class("com/example/CachedDependency"),
        )],
    );
    let resolved = vec![ResolvedClasspath {
        module_name: "cached".to_string(),
        module_root: project_root.to_path_buf(),
        entries: vec![ClasspathEntry {
            jar_path,
            coordinates: Some("com.example:cached-dependency:1.0.0".to_string()),
            is_direct: true,
            source_jar: None,
        }],
    }];
    let cache_dir = project_root.join(".sqry/classpath");
    std::fs::create_dir_all(&cache_dir).unwrap();
    std::fs::write(
        cache_dir.join(cache_file_name),
        serde_json::to_string(&resolved).unwrap(),
    )
    .unwrap();
}

// ═══════════════════════════════════════════════════════════════════════════
// 1. Build System Detection Tests (from fixture projects)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn detect_gradle_single_module() {
    let fixture = fixtures_dir().join("gradle-single-module");
    assert!(fixture.exists(), "fixture not found: {}", fixture.display());

    let result = detect_build_system(&fixture, None);
    assert_eq!(result.build_system, Some(BuildSystem::Gradle));
    assert!(
        result.markers_found.contains(&"build.gradle".to_string()),
        "expected build.gradle marker, found: {:?}",
        result.markers_found
    );
}

#[test]
fn detect_gradle_multi_module() {
    let fixture = fixtures_dir().join("gradle-multi-module");
    assert!(fixture.exists(), "fixture not found: {}", fixture.display());

    let result = detect_build_system(&fixture, None);
    assert_eq!(result.build_system, Some(BuildSystem::Gradle));
    assert!(
        result
            .markers_found
            .contains(&"settings.gradle".to_string()),
        "expected settings.gradle marker, found: {:?}",
        result.markers_found
    );
    assert!(
        result.markers_found.contains(&"build.gradle".to_string()),
        "expected build.gradle marker in multi-module root"
    );
}

#[test]
fn detect_maven_single_module() {
    let fixture = fixtures_dir().join("maven-single-module");
    assert!(fixture.exists(), "fixture not found: {}", fixture.display());

    let result = detect_build_system(&fixture, None);
    assert_eq!(result.build_system, Some(BuildSystem::Maven));
    assert!(
        result.markers_found.contains(&"pom.xml".to_string()),
        "expected pom.xml marker"
    );
}

#[test]
fn detect_maven_multi_module() {
    let fixture = fixtures_dir().join("maven-multi-module");
    assert!(fixture.exists(), "fixture not found: {}", fixture.display());

    let result = detect_build_system(&fixture, None);
    assert_eq!(result.build_system, Some(BuildSystem::Maven));
    assert!(
        result.markers_found.contains(&"pom.xml".to_string()),
        "expected pom.xml marker"
    );
}

#[test]
fn detect_bazel_java() {
    let fixture = fixtures_dir().join("bazel-java");
    assert!(fixture.exists(), "fixture not found: {}", fixture.display());

    let result = detect_build_system(&fixture, None);
    assert_eq!(result.build_system, Some(BuildSystem::Bazel));
    assert!(
        result.markers_found.contains(&"WORKSPACE".to_string()),
        "expected WORKSPACE marker, found: {:?}",
        result.markers_found
    );
    assert!(
        result.markers_found.contains(&"BUILD".to_string()),
        "expected BUILD marker, found: {:?}",
        result.markers_found
    );
}

#[test]
fn detect_kotlin_gradle_project() {
    let fixture = fixtures_dir().join("kotlin-project");
    assert!(fixture.exists(), "fixture not found: {}", fixture.display());

    let result = detect_build_system(&fixture, None);
    assert_eq!(result.build_system, Some(BuildSystem::Gradle));
    assert!(
        result
            .markers_found
            .contains(&"build.gradle.kts".to_string()),
        "expected build.gradle.kts marker, found: {:?}",
        result.markers_found
    );
}

#[test]
fn detect_scala_sbt_project() {
    let fixture = fixtures_dir().join("scala-project");
    assert!(fixture.exists(), "fixture not found: {}", fixture.display());

    let result = detect_build_system(&fixture, None);
    assert_eq!(result.build_system, Some(BuildSystem::Sbt));
    assert!(
        result.markers_found.contains(&"build.sbt".to_string()),
        "expected build.sbt marker, found: {:?}",
        result.markers_found
    );
}

#[test]
fn detect_no_build_system_in_empty_dir() {
    let tmp = TempDir::new().unwrap();
    let result = detect_build_system(tmp.path(), None);
    assert_eq!(
        result.build_system, None,
        "empty directory should detect no build system"
    );
    assert!(
        result.markers_found.is_empty(),
        "no markers should be found in empty directory"
    );
}

#[test]
fn detect_override_takes_precedence() {
    let fixture = fixtures_dir().join("maven-single-module");
    // Even though pom.xml is present, override should force Gradle.
    let result = detect_build_system(&fixture, Some("gradle"));
    assert_eq!(result.build_system, Some(BuildSystem::Gradle));
    assert_eq!(result.override_source, Some("gradle".to_string()));
    // Override skips marker scanning entirely.
    assert!(
        result.markers_found.is_empty(),
        "override should skip marker scanning"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 2. Pipeline with Manual Classpath File (no subprocess)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn pipeline_manual_classpath_single_jar() {
    let tmp = TempDir::new().unwrap();

    let class_a = build_minimal_class("com/example/Alpha");
    let class_b = build_minimal_class("com/example/Beta");
    let jar_path = write_test_jar(
        tmp.path(),
        "deps.jar",
        &[
            ("com/example/Alpha.class", &class_a),
            ("com/example/Beta.class", &class_b),
        ],
    );

    let cp_file = write_classpath_file(tmp.path(), &[jar_path.as_path()]);

    let config = ClasspathConfig {
        enabled: true,
        depth: ClasspathDepth::Full,
        build_system_override: None,
        classpath_file: Some(cp_file),
        force: false,
        timeout_secs: 30,
        allow_build_tool_execution: true,
    };

    let result = run_classpath_pipeline(tmp.path(), &config).unwrap();
    assert_eq!(result.jars_scanned, 1);
    assert_eq!(result.classes_parsed, 2);
    assert!(result.index.lookup_fqn("com.example.Alpha").is_some());
    assert!(result.index.lookup_fqn("com.example.Beta").is_some());
    assert!(
        result.index.lookup_fqn("com.example.Gamma").is_none(),
        "non-existent class should not be found"
    );
}

#[test]
fn pipeline_manual_classpath_multiple_jars() {
    let tmp = TempDir::new().unwrap();

    let class_a = build_minimal_class("com/example/Alpha");
    let class_b = build_minimal_class("org/other/Beta");
    let class_c = build_minimal_class("org/other/Gamma");

    let jar_a = write_test_jar(
        tmp.path(),
        "a.jar",
        &[("com/example/Alpha.class", &class_a)],
    );
    let jar_b = write_test_jar(
        tmp.path(),
        "b.jar",
        &[
            ("org/other/Beta.class", &class_b),
            ("org/other/Gamma.class", &class_c),
        ],
    );

    let cp_file = write_classpath_file(tmp.path(), &[jar_a.as_path(), jar_b.as_path()]);

    let config = ClasspathConfig {
        enabled: true,
        depth: ClasspathDepth::Full,
        build_system_override: None,
        classpath_file: Some(cp_file),
        force: false,
        timeout_secs: 30,
        allow_build_tool_execution: true,
    };

    let result = run_classpath_pipeline(tmp.path(), &config).unwrap();
    assert_eq!(result.jars_scanned, 2);
    assert_eq!(result.classes_parsed, 3);
    assert!(result.index.lookup_fqn("com.example.Alpha").is_some());
    assert!(result.index.lookup_fqn("org.other.Beta").is_some());
    assert!(result.index.lookup_fqn("org.other.Gamma").is_some());
    assert_eq!(result.provenance.len(), 2);
}

#[test]
fn pipeline_manual_classpath_with_comments_and_blanks() {
    let tmp = TempDir::new().unwrap();

    let class_a = build_minimal_class("com/example/Only");
    let jar_path = write_test_jar(
        tmp.path(),
        "only.jar",
        &[("com/example/Only.class", &class_a)],
    );

    let cp_file = tmp.path().join("classpath.txt");
    std::fs::write(
        &cp_file,
        format!(
            "# Comment line\n\n{}\n\n# Another comment\n",
            jar_path.display()
        ),
    )
    .unwrap();

    let config = ClasspathConfig {
        enabled: true,
        depth: ClasspathDepth::Full,
        build_system_override: None,
        classpath_file: Some(cp_file),
        force: false,
        timeout_secs: 30,
        allow_build_tool_execution: true,
    };

    let result = run_classpath_pipeline(tmp.path(), &config).unwrap();
    assert_eq!(result.jars_scanned, 1);
    assert_eq!(result.classes_parsed, 1);
    assert!(result.index.lookup_fqn("com.example.Only").is_some());
}

#[test]
fn pipeline_no_build_system_returns_detection_error() {
    let tmp = TempDir::new().unwrap();
    let config = ClasspathConfig {
        enabled: true,
        ..ClasspathConfig::default()
    };

    let result = run_classpath_pipeline(tmp.path(), &config);
    assert!(result.is_err(), "empty project should fail detection");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("No JVM build system detected"),
        "expected detection error, got: {err}"
    );
}

#[test]
fn pipeline_empty_classpath_file() {
    let tmp = TempDir::new().unwrap();

    let cp_file = tmp.path().join("classpath.txt");
    std::fs::write(&cp_file, "# Only comments\n\n").unwrap();

    let config = ClasspathConfig {
        enabled: true,
        depth: ClasspathDepth::Full,
        build_system_override: None,
        classpath_file: Some(cp_file),
        force: false,
        timeout_secs: 30,
        allow_build_tool_execution: true,
    };

    let result = run_classpath_pipeline(tmp.path(), &config).unwrap();
    assert_eq!(result.jars_scanned, 0);
    assert_eq!(result.classes_parsed, 0);
    assert_eq!(result.index.classes.len(), 0);
}

#[test]
fn pipeline_nonexistent_classpath_file_returns_error() {
    let tmp = TempDir::new().unwrap();
    let config = ClasspathConfig {
        enabled: true,
        classpath_file: Some(tmp.path().join("nonexistent.txt")),
        ..ClasspathConfig::default()
    };

    let result = run_classpath_pipeline(tmp.path(), &config);
    assert!(result.is_err(), "nonexistent classpath file should fail");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("Cannot open classpath file"),
        "expected file open error, got: {err}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 3. Index Persistence Roundtrip
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn index_persistence_roundtrip_empty() {
    let tmp = TempDir::new().unwrap();
    let index_path = tmp.path().join("index.sqry");

    let index = ClasspathIndex::build(vec![]);
    index.save(&index_path).unwrap();

    let loaded = ClasspathIndex::load(&index_path).unwrap();
    assert_eq!(loaded.classes.len(), 0);
    assert!(loaded.package_index.is_empty());
    assert!(loaded.annotation_index.is_empty());
}

#[test]
fn index_persistence_roundtrip_with_classes() {
    let tmp = TempDir::new().unwrap();
    let index_path = tmp.path().join("classpath/index.sqry");

    let stubs = vec![
        make_stub("com.example.Alpha"),
        make_stub("com.example.Beta"),
        make_stub("java.util.HashMap"),
        make_stub("java.util.ArrayList"),
        make_stub("org.slf4j.Logger"),
    ];

    let index = ClasspathIndex::build(stubs);
    index.save(&index_path).unwrap();

    let loaded = ClasspathIndex::load(&index_path).unwrap();
    assert_eq!(loaded.classes.len(), 5);

    // Verify sorting is preserved after roundtrip.
    for window in loaded.classes.windows(2) {
        assert!(
            window[0].fqn <= window[1].fqn,
            "sort order violated after roundtrip: {} > {}",
            window[0].fqn,
            window[1].fqn
        );
    }

    // Verify FQN lookup works after reload.
    assert!(loaded.lookup_fqn("com.example.Alpha").is_some());
    assert!(loaded.lookup_fqn("java.util.HashMap").is_some());
    assert!(loaded.lookup_fqn("org.slf4j.Logger").is_some());
    assert!(loaded.lookup_fqn("does.not.Exist").is_none());
}

#[test]
fn index_persistence_roundtrip_with_annotations() {
    let tmp = TempDir::new().unwrap();
    let index_path = tmp.path().join("index.sqry");

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
    index.save(&index_path).unwrap();

    let loaded = ClasspathIndex::load(&index_path).unwrap();
    let controllers = loaded.lookup_annotated("org.springframework.stereotype.Controller");
    assert_eq!(
        controllers.len(),
        2,
        "should find 2 controller-annotated classes after roundtrip"
    );

    let rest_controllers =
        loaded.lookup_annotated("org.springframework.web.bind.annotation.RestController");
    assert_eq!(
        rest_controllers.len(),
        1,
        "should find 1 RestController-annotated class after roundtrip"
    );
}

#[test]
fn index_persistence_roundtrip_via_pipeline() {
    let tmp = TempDir::new().unwrap();

    let class_a = build_minimal_class("com/example/Roundtrip");
    let jar_path = write_test_jar(
        tmp.path(),
        "rt.jar",
        &[("com/example/Roundtrip.class", &class_a)],
    );
    let cp_file = write_classpath_file(tmp.path(), &[jar_path.as_path()]);

    let config = ClasspathConfig {
        enabled: true,
        depth: ClasspathDepth::Full,
        classpath_file: Some(cp_file),
        force: false,
        timeout_secs: 30,
        ..ClasspathConfig::default()
    };

    // Run pipeline — it persists the index.
    let result = run_classpath_pipeline(tmp.path(), &config).unwrap();
    assert_eq!(result.classes_parsed, 1);

    // Load the persisted index directly.
    let index_path = tmp.path().join(".sqry/classpath/index.sqry");
    assert!(
        index_path.exists(),
        "pipeline should persist index at .sqry/classpath/index.sqry"
    );

    let loaded = ClasspathIndex::load(&index_path).unwrap();
    assert_eq!(loaded.classes.len(), 1);
    assert_eq!(loaded.classes[0].fqn, "com.example.Roundtrip");

    // Verify provenance was also persisted.
    let prov_path = tmp.path().join(".sqry/classpath/provenance.json");
    assert!(
        prov_path.exists(),
        "pipeline should persist provenance at .sqry/classpath/provenance.json"
    );
    let prov_json = std::fs::read_to_string(&prov_path).unwrap();
    let prov: serde_json::Value = serde_json::from_str(&prov_json).unwrap();
    assert!(prov.is_array());
    assert_eq!(prov.as_array().unwrap().len(), 1);
}

// ═══════════════════════════════════════════════════════════════════════════
// 4. FQN Workspace Shadowing
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn workspace_fqn_shadows_classpath_in_index() {
    // Simulate a classpath index where a workspace-defined class would shadow
    // a classpath-provided one. The classpath index itself does not distinguish
    // workspace vs. classpath — that is the graph emitter's responsibility.
    // However, we verify that if the same FQN appears in two different stubs,
    // the index correctly deduplicates via sorted order (last-writer-wins in
    // build order, but binary search returns the first match).

    let classpath_stub = make_stub_with_methods("com.example.AppService", &["handleRequest"]);
    let workspace_stub =
        make_stub_with_methods("com.example.AppService", &["handleRequest", "initialize"]);

    // Building with both — the index should contain both, but lookup_fqn
    // returns the first match (which is deterministic since sort is stable).
    let index = ClasspathIndex::build(vec![classpath_stub, workspace_stub]);

    // The index does not deduplicate same-FQN stubs; both are present.
    let all_in_package = index.lookup_package("com.example");
    assert_eq!(
        all_in_package.len(),
        2,
        "both stubs with same FQN should be in the index"
    );

    // lookup_fqn returns the first binary search match.
    let found = index.lookup_fqn("com.example.AppService");
    assert!(found.is_some(), "should find AppService by FQN");
}

#[test]
fn workspace_shadow_different_packages() {
    // Workspace provides com.example.Foo, classpath provides org.library.Foo.
    // Both should be independently resolvable.
    let ws_stub = make_stub("com.example.Foo");
    let cp_stub = make_stub("org.library.Foo");

    let index = ClasspathIndex::build(vec![ws_stub, cp_stub]);

    assert!(index.lookup_fqn("com.example.Foo").is_some());
    assert!(index.lookup_fqn("org.library.Foo").is_some());

    let com_example = index.lookup_package("com.example");
    assert_eq!(com_example.len(), 1);
    assert_eq!(com_example[0].fqn, "com.example.Foo");

    let org_library = index.lookup_package("org.library");
    assert_eq!(org_library.len(), 1);
    assert_eq!(org_library[0].fqn, "org.library.Foo");
}

// ═══════════════════════════════════════════════════════════════════════════
// 5. Package Lookup
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn package_lookup_groups_correctly() {
    let stubs = vec![
        make_stub("com.example.Alpha"),
        make_stub("com.example.Beta"),
        make_stub("com.example.Gamma"),
        make_stub("java.util.HashMap"),
        make_stub("java.util.ArrayList"),
        make_stub("java.util.LinkedList"),
        make_stub("org.slf4j.Logger"),
    ];

    let index = ClasspathIndex::build(stubs);

    let com_example = index.lookup_package("com.example");
    assert_eq!(com_example.len(), 3);
    let fqns: Vec<&str> = com_example.iter().map(|s| s.fqn.as_str()).collect();
    assert!(fqns.contains(&"com.example.Alpha"));
    assert!(fqns.contains(&"com.example.Beta"));
    assert!(fqns.contains(&"com.example.Gamma"));

    let java_util = index.lookup_package("java.util");
    assert_eq!(java_util.len(), 3);

    let org_slf4j = index.lookup_package("org.slf4j");
    assert_eq!(org_slf4j.len(), 1);
    assert_eq!(org_slf4j[0].fqn, "org.slf4j.Logger");

    let nonexistent = index.lookup_package("does.not.exist");
    assert!(nonexistent.is_empty());
}

#[test]
fn package_lookup_default_package() {
    let stubs = vec![make_stub("NoPackageClass"), make_stub("AnotherDefault")];

    let index = ClasspathIndex::build(stubs);

    // Default package is the empty string.
    let default_pkg = index.lookup_package("");
    assert_eq!(default_pkg.len(), 2);
}

#[test]
fn package_lookup_deeply_nested() {
    let stubs = vec![
        make_stub("com.example.deep.nested.pkg.ClassA"),
        make_stub("com.example.deep.nested.pkg.ClassB"),
        make_stub("com.example.deep.other.ClassC"),
    ];

    let index = ClasspathIndex::build(stubs);

    let nested = index.lookup_package("com.example.deep.nested.pkg");
    assert_eq!(nested.len(), 2);

    let other = index.lookup_package("com.example.deep.other");
    assert_eq!(other.len(), 1);

    // Partial package should not match.
    let partial = index.lookup_package("com.example.deep");
    assert!(
        partial.is_empty(),
        "partial package path should not match any classes"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 6. Annotation Lookup
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn annotation_lookup_single_annotation() {
    let stubs = vec![
        make_annotated_stub("com.example.UserService", &["javax.inject.Singleton"]),
        make_annotated_stub("com.example.OrderService", &["javax.inject.Singleton"]),
        make_stub("com.example.PlainClass"), // No annotations.
    ];

    let index = ClasspathIndex::build(stubs);

    let singletons = index.lookup_annotated("javax.inject.Singleton");
    assert_eq!(singletons.len(), 2);
    let fqns: Vec<&str> = singletons.iter().map(|s| s.fqn.as_str()).collect();
    assert!(fqns.contains(&"com.example.UserService"));
    assert!(fqns.contains(&"com.example.OrderService"));

    // Non-annotated classes should not appear.
    let plain = index.lookup_annotated("non.existent.Annotation");
    assert!(plain.is_empty());
}

#[test]
fn annotation_lookup_multiple_annotations_per_class() {
    let stubs = vec![make_annotated_stub(
        "com.example.RestEndpoint",
        &[
            "javax.ws.rs.Path",
            "javax.inject.Singleton",
            "javax.enterprise.context.RequestScoped",
        ],
    )];

    let index = ClasspathIndex::build(stubs);

    // Each annotation should index back to the same class.
    assert_eq!(index.lookup_annotated("javax.ws.rs.Path").len(), 1);
    assert_eq!(index.lookup_annotated("javax.inject.Singleton").len(), 1);
    assert_eq!(
        index
            .lookup_annotated("javax.enterprise.context.RequestScoped")
            .len(),
        1
    );

    // All should point to the same class.
    let found = index.lookup_annotated("javax.ws.rs.Path");
    assert_eq!(found[0].fqn, "com.example.RestEndpoint");
}

#[test]
fn annotation_lookup_spring_framework_pattern() {
    // Simulate a typical Spring Boot project's classpath with controllers,
    // services, and repositories.
    let stubs = vec![
        make_annotated_stub(
            "com.example.web.UserController",
            &["org.springframework.web.bind.annotation.RestController"],
        ),
        make_annotated_stub(
            "com.example.web.OrderController",
            &["org.springframework.web.bind.annotation.RestController"],
        ),
        make_annotated_stub(
            "com.example.service.UserService",
            &["org.springframework.stereotype.Service"],
        ),
        make_annotated_stub(
            "com.example.repo.UserRepository",
            &["org.springframework.stereotype.Repository"],
        ),
        make_annotated_stub(
            "com.example.config.AppConfig",
            &["org.springframework.context.annotation.Configuration"],
        ),
    ];

    let index = ClasspathIndex::build(stubs);

    let controllers =
        index.lookup_annotated("org.springframework.web.bind.annotation.RestController");
    assert_eq!(controllers.len(), 2);

    let services = index.lookup_annotated("org.springframework.stereotype.Service");
    assert_eq!(services.len(), 1);
    assert_eq!(services[0].fqn, "com.example.service.UserService");

    let repos = index.lookup_annotated("org.springframework.stereotype.Repository");
    assert_eq!(repos.len(), 1);

    let configs = index.lookup_annotated("org.springframework.context.annotation.Configuration");
    assert_eq!(configs.len(), 1);
}

// ═══════════════════════════════════════════════════════════════════════════
// 7. Bytecode Scanning Integration (via test JARs)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn scan_jar_via_pipeline_produces_correct_stubs() {
    let tmp = TempDir::new().unwrap();

    // Build classes with various package depths.
    let class_root = build_minimal_class("RootClass");
    let class_single = build_minimal_class("com/Example");
    let class_deep = build_minimal_class("com/example/deep/nested/DeepClass");

    let jar_path = write_test_jar(
        tmp.path(),
        "mixed.jar",
        &[
            ("RootClass.class", &class_root),
            ("com/Example.class", &class_single),
            ("com/example/deep/nested/DeepClass.class", &class_deep),
        ],
    );

    let cp_file = write_classpath_file(tmp.path(), &[jar_path.as_path()]);

    let config = ClasspathConfig {
        enabled: true,
        depth: ClasspathDepth::Full,
        classpath_file: Some(cp_file),
        force: false,
        timeout_secs: 30,
        ..ClasspathConfig::default()
    };

    let result = run_classpath_pipeline(tmp.path(), &config).unwrap();
    assert_eq!(result.classes_parsed, 3);

    // Verify each class is accessible by FQN (dots, not slashes).
    assert!(result.index.lookup_fqn("RootClass").is_some());
    assert!(result.index.lookup_fqn("com.Example").is_some());
    assert!(
        result
            .index
            .lookup_fqn("com.example.deep.nested.DeepClass")
            .is_some()
    );

    // Verify package index for the deep class.
    let deep_pkg = result.index.lookup_package("com.example.deep.nested");
    assert_eq!(deep_pkg.len(), 1);
    assert_eq!(deep_pkg[0].fqn, "com.example.deep.nested.DeepClass");
}

#[test]
fn scan_jar_ignores_non_class_entries() {
    let tmp = TempDir::new().unwrap();

    let class_a = build_minimal_class("com/example/Valid");

    let jar_path = write_test_jar(
        tmp.path(),
        "mixed-content.jar",
        &[
            ("com/example/Valid.class", &class_a),
            ("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\n"),
            ("README.txt", b"This is not a class file.\n"),
            ("resources/config.properties", b"key=value\n"),
        ],
    );

    let cp_file = write_classpath_file(tmp.path(), &[jar_path.as_path()]);

    let config = ClasspathConfig {
        enabled: true,
        depth: ClasspathDepth::Full,
        classpath_file: Some(cp_file),
        force: false,
        timeout_secs: 30,
        ..ClasspathConfig::default()
    };

    let result = run_classpath_pipeline(tmp.path(), &config).unwrap();
    assert_eq!(
        result.classes_parsed, 1,
        "only .class entries should be parsed"
    );
    assert!(result.index.lookup_fqn("com.example.Valid").is_some());
}

// ═══════════════════════════════════════════════════════════════════════════
// 8. Index with Methods and Fields
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn index_preserves_method_stubs() {
    let tmp = TempDir::new().unwrap();
    let index_path = tmp.path().join("index.sqry");

    let stubs = vec![make_stub_with_methods(
        "com.example.Service",
        &["doWork", "initialize", "shutdown"],
    )];

    let index = ClasspathIndex::build(stubs);
    index.save(&index_path).unwrap();

    let loaded = ClasspathIndex::load(&index_path).unwrap();
    let service = loaded.lookup_fqn("com.example.Service").unwrap();
    assert_eq!(service.methods.len(), 3);
    let method_names: Vec<&str> = service.methods.iter().map(|m| m.name.as_str()).collect();
    assert!(method_names.contains(&"doWork"));
    assert!(method_names.contains(&"initialize"));
    assert!(method_names.contains(&"shutdown"));
}

#[test]
fn index_preserves_field_stubs() {
    let tmp = TempDir::new().unwrap();
    let index_path = tmp.path().join("index.sqry");

    let stubs = vec![make_stub_with_fields(
        "com.example.Config",
        &[
            ("port", "I"),
            ("host", "Ljava/lang/String;"),
            ("enabled", "Z"),
        ],
    )];

    let index = ClasspathIndex::build(stubs);
    index.save(&index_path).unwrap();

    let loaded = ClasspathIndex::load(&index_path).unwrap();
    let config = loaded.lookup_fqn("com.example.Config").unwrap();
    assert_eq!(config.fields.len(), 3);
    let field_names: Vec<&str> = config.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(field_names.contains(&"port"));
    assert!(field_names.contains(&"host"));
    assert!(field_names.contains(&"enabled"));
}

// ═══════════════════════════════════════════════════════════════════════════
// 9. Class Kind Variants
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn index_handles_various_class_kinds() {
    let mut interface_stub = make_stub("com.example.Service");
    interface_stub.kind = ClassKind::Interface;

    let mut enum_stub = make_stub("com.example.Color");
    enum_stub.kind = ClassKind::Enum;

    let mut annotation_stub = make_stub("com.example.MyAnnotation");
    annotation_stub.kind = ClassKind::Annotation;

    let class_stub = make_stub("com.example.Impl");

    let index = ClasspathIndex::build(vec![interface_stub, enum_stub, annotation_stub, class_stub]);

    assert_eq!(index.classes.len(), 4);

    let service = index.lookup_fqn("com.example.Service").unwrap();
    assert_eq!(service.kind, ClassKind::Interface);

    let color = index.lookup_fqn("com.example.Color").unwrap();
    assert_eq!(color.kind, ClassKind::Enum);

    let ann = index.lookup_fqn("com.example.MyAnnotation").unwrap();
    assert_eq!(ann.kind, ClassKind::Annotation);

    let imp = index.lookup_fqn("com.example.Impl").unwrap();
    assert_eq!(imp.kind, ClassKind::Class);
}

// ═══════════════════════════════════════════════════════════════════════════
// 10. Pipeline Cache Behavior
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn pipeline_second_run_uses_cache() {
    let tmp = TempDir::new().unwrap();

    let class_a = build_minimal_class("com/cache/CacheTest");
    let jar_path = write_test_jar(
        tmp.path(),
        "cache-test.jar",
        &[("com/cache/CacheTest.class", &class_a)],
    );
    let cp_file = write_classpath_file(tmp.path(), &[jar_path.as_path()]);

    let config = ClasspathConfig {
        enabled: true,
        depth: ClasspathDepth::Full,
        classpath_file: Some(cp_file.clone()),
        force: false,
        timeout_secs: 30,
        ..ClasspathConfig::default()
    };

    // First run: fresh scan.
    let result1 = run_classpath_pipeline(tmp.path(), &config).unwrap();
    assert!(!result1.from_cache, "first run should not be from cache");
    assert_eq!(result1.classes_parsed, 1);

    // Second run: should use cache.
    let config2 = ClasspathConfig {
        enabled: true,
        depth: ClasspathDepth::Full,
        classpath_file: Some(cp_file),
        force: false,
        timeout_secs: 30,
        ..ClasspathConfig::default()
    };
    let result2 = run_classpath_pipeline(tmp.path(), &config2).unwrap();
    assert!(result2.from_cache, "second run should use cached stubs");
    assert_eq!(result2.classes_parsed, 1);
    assert!(
        result2.index.lookup_fqn("com.cache.CacheTest").is_some(),
        "cached result should still find classes"
    );
}

#[test]
fn pipeline_force_bypasses_cache() {
    let tmp = TempDir::new().unwrap();

    let class_a = build_minimal_class("com/force/ForceTest");
    let jar_path = write_test_jar(
        tmp.path(),
        "force-test.jar",
        &[("com/force/ForceTest.class", &class_a)],
    );
    let cp_file = write_classpath_file(tmp.path(), &[jar_path.as_path()]);

    // First run: populate cache.
    let config = ClasspathConfig {
        enabled: true,
        depth: ClasspathDepth::Full,
        classpath_file: Some(cp_file.clone()),
        force: false,
        timeout_secs: 30,
        ..ClasspathConfig::default()
    };
    let _ = run_classpath_pipeline(tmp.path(), &config).unwrap();

    // Second run with force: should NOT use cache.
    let config_force = ClasspathConfig {
        enabled: true,
        depth: ClasspathDepth::Full,
        classpath_file: Some(cp_file),
        force: true,
        timeout_secs: 30,
        ..ClasspathConfig::default()
    };
    let result = run_classpath_pipeline(tmp.path(), &config_force).unwrap();
    assert!(!result.from_cache, "force=true should bypass cache");
    assert_eq!(result.classes_parsed, 1);
}

// ═══════════════════════════════════════════════════════════════════════════
// 11. Large Index Stress Test
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn large_index_build_and_lookup() {
    let stubs: Vec<ClassStub> = (0..2000)
        .map(|i| {
            let pkg = match i % 4 {
                0 => "com.example.alpha",
                1 => "com.example.beta",
                2 => "org.library.core",
                _ => "io.framework.util",
            };
            make_stub(&format!("{pkg}.Class{i:04}"))
        })
        .collect();

    let index = ClasspathIndex::build(stubs);
    assert_eq!(index.classes.len(), 2000);

    // Verify sort order.
    for window in index.classes.windows(2) {
        assert!(
            window[0].fqn <= window[1].fqn,
            "sort violation: {} > {}",
            window[0].fqn,
            window[1].fqn
        );
    }

    // Spot-check lookups.
    assert!(index.lookup_fqn("com.example.alpha.Class0000").is_some());
    assert!(index.lookup_fqn("org.library.core.Class0002").is_some());
    assert!(index.lookup_fqn("io.framework.util.Class1999").is_some());
    assert!(index.lookup_fqn("com.example.alpha.Class9999").is_none());

    // Package grouping.
    let alpha = index.lookup_package("com.example.alpha");
    assert_eq!(alpha.len(), 500);
    let beta = index.lookup_package("com.example.beta");
    assert_eq!(beta.len(), 500);
}

// ═══════════════════════════════════════════════════════════════════════════
// 12. Build System Detection with Override (integration)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn detect_override_all_build_systems() {
    let tmp = TempDir::new().unwrap();

    for (name, expected) in [
        ("gradle", BuildSystem::Gradle),
        ("maven", BuildSystem::Maven),
        ("bazel", BuildSystem::Bazel),
        ("sbt", BuildSystem::Sbt),
    ] {
        let result = detect_build_system(tmp.path(), Some(name));
        assert_eq!(
            result.build_system,
            Some(expected),
            "override '{name}' should detect {expected:?}"
        );
    }
}

#[test]
fn detect_override_case_insensitive() {
    let tmp = TempDir::new().unwrap();

    for name in [
        "GRADLE", "Gradle", "gradle", "MAVEN", "Maven", "BAZEL", "SBT",
    ] {
        let result = detect_build_system(tmp.path(), Some(name));
        assert!(
            result.build_system.is_some(),
            "case-insensitive override '{name}' should succeed"
        );
    }
}

#[test]
fn detect_override_invalid_returns_none() {
    let tmp = TempDir::new().unwrap();
    let result = detect_build_system(tmp.path(), Some("cmake"));
    assert_eq!(
        result.build_system, None,
        "invalid override should return None"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 13. Full pipeline fallback tests for build-system-resolved classpaths.
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn pipeline_gradle_single_module_real() {
    let project = copy_fixture_to_temp("gradle-single-module");
    write_cached_resolved_classpath(project.path(), "resolved-classpath.json");
    let config = ClasspathConfig {
        enabled: true,
        depth: ClasspathDepth::Full,
        build_system_override: None,
        classpath_file: None,
        force: true,
        timeout_secs: 120,
        allow_build_tool_execution: true,
    };

    let result = run_classpath_pipeline(project.path(), &config).unwrap();
    assert!(
        result.jars_scanned > 0,
        "should scan at least one JAR from Gradle"
    );
    assert!(
        result.classes_parsed > 0,
        "should parse at least one class from Gradle dependencies"
    );
}

#[test]
fn pipeline_maven_single_module_real() {
    let project = copy_fixture_to_temp("maven-single-module");
    write_cached_resolved_classpath(project.path(), "maven-resolved-classpath.json");
    let config = ClasspathConfig {
        enabled: true,
        depth: ClasspathDepth::Full,
        build_system_override: None,
        classpath_file: None,
        force: true,
        timeout_secs: 120,
        allow_build_tool_execution: true,
    };

    let result = run_classpath_pipeline(project.path(), &config).unwrap();
    assert!(
        result.jars_scanned > 0,
        "should scan at least one JAR from Maven"
    );
}

#[test]
fn pipeline_bazel_java_real() {
    let project = copy_fixture_to_temp("bazel-java");
    write_cached_resolved_classpath(project.path(), "bazel-resolved-classpath.json");
    let config = ClasspathConfig {
        enabled: true,
        depth: ClasspathDepth::Full,
        build_system_override: None,
        classpath_file: None,
        force: true,
        timeout_secs: 120,
        allow_build_tool_execution: true,
    };

    let result = run_classpath_pipeline(project.path(), &config).unwrap();
    assert!(
        result.jars_scanned > 0,
        "should scan at least one JAR from Bazel"
    );
}

#[test]
fn pipeline_kotlin_project_real() {
    let project = copy_fixture_to_temp("kotlin-project");
    write_cached_resolved_classpath(project.path(), "resolved-classpath.json");
    let config = ClasspathConfig {
        enabled: true,
        depth: ClasspathDepth::Full,
        build_system_override: None,
        classpath_file: None,
        force: true,
        timeout_secs: 120,
        allow_build_tool_execution: true,
    };

    let result = run_classpath_pipeline(project.path(), &config).unwrap();
    assert!(
        result.jars_scanned > 0,
        "should scan at least one JAR from Kotlin/Gradle"
    );
}

#[test]
fn pipeline_scala_project_real() {
    let project = copy_fixture_to_temp("scala-project");
    write_cached_resolved_classpath(project.path(), "sbt-resolved-classpath.json");
    let config = ClasspathConfig {
        enabled: true,
        depth: ClasspathDepth::Full,
        build_system_override: None,
        classpath_file: None,
        force: true,
        timeout_secs: 120,
        allow_build_tool_execution: true,
    };

    let result = run_classpath_pipeline(project.path(), &config).unwrap();
    assert!(
        result.jars_scanned > 0,
        "should scan at least one JAR from sbt"
    );
}
