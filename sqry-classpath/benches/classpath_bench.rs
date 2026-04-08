//! Criterion benchmarks for the classpath pipeline.
//!
//! Targets spec Section 9 performance budgets:
//! - Cold scan (no cache): <20s for 100 JARs
//! - Warm scan (with cache): <3s for 100 JARs
//! - Incremental (1 changed JAR): <1s
//!
//! All benchmarks use synthetic class files to avoid requiring a JDK at CI time.

use std::io::Write;
use std::path::PathBuf;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use sqry_classpath::bytecode::classfile::parse_class;
use sqry_classpath::bytecode::generics::{
    parse_class_signature, parse_field_signature, parse_method_signature,
};
use sqry_classpath::bytecode::scan_jar;
use sqry_classpath::stub::cache::StubCache;
use sqry_classpath::stub::index::ClasspathIndex;
use sqry_classpath::stub::model::{AccessFlags, ClassKind, ClassStub};
use zip::write::SimpleFileOptions;

// ---------------------------------------------------------------------------
// Synthetic class-file generation
// ---------------------------------------------------------------------------

/// Build a minimal valid `.class` file for benchmarking.
///
/// The generated class has `ACC_PUBLIC | ACC_SUPER`, extends `java/lang/Object`,
/// and has no methods, fields, or attributes.
fn generate_test_class(class_name: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(128);

    // Magic
    bytes.extend_from_slice(&0xCAFE_BABEu32.to_be_bytes());
    // Minor version
    bytes.extend_from_slice(&0u16.to_be_bytes());
    // Major version (52 = Java 8)
    bytes.extend_from_slice(&52u16.to_be_bytes());

    // Constant pool: 4 entries + 1 (cp_count = 5)
    //   #1 Utf8   <class_name>
    //   #2 Class  -> #1
    //   #3 Utf8   "java/lang/Object"
    //   #4 Class  -> #3
    let class_bytes = class_name.as_bytes();
    let object_bytes = b"java/lang/Object";
    let cp_count: u16 = 5;
    bytes.extend_from_slice(&cp_count.to_be_bytes());

    // #1 CONSTANT_Utf8
    bytes.push(1);
    #[expect(clippy::cast_possible_truncation)]
    let class_len = class_bytes.len() as u16;
    bytes.extend_from_slice(&class_len.to_be_bytes());
    bytes.extend_from_slice(class_bytes);

    // #2 CONSTANT_Class -> #1
    bytes.push(7);
    bytes.extend_from_slice(&1u16.to_be_bytes());

    // #3 CONSTANT_Utf8 "java/lang/Object"
    bytes.push(1);
    #[expect(clippy::cast_possible_truncation)]
    let obj_len = object_bytes.len() as u16;
    bytes.extend_from_slice(&obj_len.to_be_bytes());
    bytes.extend_from_slice(object_bytes);

    // #4 CONSTANT_Class -> #3
    bytes.push(7);
    bytes.extend_from_slice(&3u16.to_be_bytes());

    // Access flags: ACC_PUBLIC | ACC_SUPER
    bytes.extend_from_slice(&0x0021u16.to_be_bytes());
    // This class: #2
    bytes.extend_from_slice(&2u16.to_be_bytes());
    // Super class: #4
    bytes.extend_from_slice(&4u16.to_be_bytes());
    // Interfaces count
    bytes.extend_from_slice(&0u16.to_be_bytes());
    // Fields count
    bytes.extend_from_slice(&0u16.to_be_bytes());
    // Methods count
    bytes.extend_from_slice(&0u16.to_be_bytes());
    // Attributes count
    bytes.extend_from_slice(&0u16.to_be_bytes());

    bytes
}

/// Generate a class name in the form `com/example/pkg{pkg}/Class{idx}`.
fn class_entry_name(pkg: usize, idx: usize) -> String {
    format!("com/example/pkg{pkg}/Class{idx}.class")
}

/// Generate a FQN in the form `com.example.pkg{pkg}.Class{idx}`.
fn class_fqn(pkg: usize, idx: usize) -> String {
    format!("com.example.pkg{pkg}.Class{idx}")
}

/// Create a temporary JAR file containing `class_count` synthetic classes
/// spread across 10 packages.
///
/// Returns the `TempDir` (to keep it alive for the benchmark) and the JAR path.
fn generate_test_jar(class_count: usize) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("create tempdir");
    let jar_path = dir.path().join("bench.jar");

    let file = std::fs::File::create(&jar_path).expect("create JAR file");
    let mut writer = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

    let packages = 10;
    for i in 0..class_count {
        let pkg = i % packages;
        let entry_name = class_entry_name(pkg, i);
        let internal_name = format!("com/example/pkg{pkg}/Class{i}");
        let class_bytes = generate_test_class(&internal_name);

        writer
            .start_file(&entry_name, options)
            .expect("start ZIP entry");
        writer.write_all(&class_bytes).expect("write class bytes");
    }

    writer.finish().expect("finish ZIP");
    (dir, jar_path)
}

/// Create a `ClassStub` for benchmarking.
fn make_bench_stub(fqn: &str) -> ClassStub {
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

/// Generate a vector of test stubs.
fn generate_test_stubs(count: usize) -> Vec<ClassStub> {
    let packages = 10;
    (0..count)
        .map(|i| {
            let pkg = i % packages;
            make_bench_stub(&class_fqn(pkg, i))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Benchmarks: class parsing
// ---------------------------------------------------------------------------

fn bench_class_parsing(c: &mut Criterion) {
    let class_bytes = generate_test_class("com/example/BenchClass");

    c.bench_function("parse_single_class", |b| {
        b.iter(|| parse_class(std::hint::black_box(&class_bytes)));
    });
}

// ---------------------------------------------------------------------------
// Benchmarks: generic signature parsing
// ---------------------------------------------------------------------------

fn bench_generic_signatures(c: &mut Criterion) {
    let mut group = c.benchmark_group("generic_signatures");

    // Simple non-generic class: just `Ljava/lang/Object;`
    let simple = "Ljava/lang/Object;";
    group.bench_with_input(BenchmarkId::new("class", "simple"), &simple, |b, sig| {
        b.iter(|| parse_class_signature(std::hint::black_box(sig)));
    });

    // HashMap-style: two type parameters, parameterized superclass + interface
    let hashmap = "<K:Ljava/lang/Object;V:Ljava/lang/Object;>Ljava/util/AbstractMap<TK;TV;>;Ljava/util/Map<TK;TV;>;";
    group.bench_with_input(BenchmarkId::new("class", "hashmap"), &hashmap, |b, sig| {
        b.iter(|| parse_class_signature(std::hint::black_box(sig)));
    });

    // Recursive bound: `<T extends Comparable<T>>`
    let comparable = "<T:Ljava/lang/Comparable<TT;>;>Ljava/lang/Object;";
    group.bench_with_input(
        BenchmarkId::new("class", "recursive_bound"),
        &comparable,
        |b, sig| b.iter(|| parse_class_signature(std::hint::black_box(sig))),
    );

    // Deeply nested: three levels of parameterization
    let nested = "Ljava/util/Map<Ljava/lang/String;Ljava/util/List<Ljava/util/Map<Ljava/lang/String;Ljava/lang/Integer;>;>;>;";
    group.bench_with_input(
        BenchmarkId::new("field", "deeply_nested"),
        &nested,
        |b, sig| b.iter(|| parse_field_signature(std::hint::black_box(sig))),
    );

    // Method signature: `<T>(T, List<T>) -> Map<String, T>`
    let method =
        "<T:Ljava/lang/Object;>(TT;Ljava/util/List<TT;>;)Ljava/util/Map<Ljava/lang/String;TT;>;";
    group.bench_with_input(
        BenchmarkId::new("method", "generic_params"),
        &method,
        |b, sig| b.iter(|| parse_method_signature(std::hint::black_box(sig))),
    );

    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmarks: JAR scanning
// ---------------------------------------------------------------------------

fn bench_jar_scanning(c: &mut Criterion) {
    let mut group = c.benchmark_group("jar_scanning");

    // Use sample_size(10) for larger JARs to keep benchmark runtime reasonable.
    group.sample_size(10);

    for &count in &[10, 50, 100, 500] {
        let (_dir, jar_path) = generate_test_jar(count);
        group.bench_with_input(BenchmarkId::new("classes", count), &jar_path, |b, path| {
            b.iter(|| scan_jar(std::hint::black_box(path)));
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmarks: ClasspathIndex build
// ---------------------------------------------------------------------------

fn bench_index_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("index_build");

    for &count in &[100, 1_000, 5_000] {
        let stubs = generate_test_stubs(count);
        group.bench_with_input(BenchmarkId::new("stubs", count), &stubs, |b, input| {
            b.iter(|| ClasspathIndex::build(std::hint::black_box(input.clone())));
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmarks: ClasspathIndex lookup
// ---------------------------------------------------------------------------

fn bench_index_lookup(c: &mut Criterion) {
    let stubs = generate_test_stubs(5_000);
    let index = ClasspathIndex::build(stubs);

    let mut group = c.benchmark_group("index_lookup");

    // FQN lookup (binary search) — hit
    let target_fqn = class_fqn(5, 2_500);
    group.bench_function("fqn_hit", |b| {
        b.iter(|| index.lookup_fqn(std::hint::black_box(&target_fqn)));
    });

    // FQN lookup — miss
    group.bench_function("fqn_miss", |b| {
        b.iter(|| index.lookup_fqn(std::hint::black_box("com.nonexistent.Missing")));
    });

    // Package lookup
    group.bench_function("package", |b| {
        b.iter(|| index.lookup_package(std::hint::black_box("com.example.pkg3")));
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmarks: StubCache read/write
// ---------------------------------------------------------------------------

fn bench_stub_cache(c: &mut Criterion) {
    let mut group = c.benchmark_group("stub_cache");

    for &count in &[10, 100, 500] {
        let stubs = generate_test_stubs(count);
        let dir = tempfile::tempdir().expect("create tempdir");
        let cache = StubCache::new(dir.path());

        // Create a JAR so the cache has something to hash.
        let (_jar_dir, jar_path) = generate_test_jar(count);

        // Warm the cache.
        cache
            .put(&jar_path, &stubs)
            .expect("cache put during setup");

        group.bench_with_input(
            BenchmarkId::new("cache_read", count),
            &(&cache, &jar_path),
            |b, (cache, path)| b.iter(|| cache.get(std::hint::black_box(path))),
        );
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmarks: StubCache write (serialization + I/O)
// ---------------------------------------------------------------------------

fn bench_stub_cache_write(c: &mut Criterion) {
    let mut group = c.benchmark_group("stub_cache_write");

    // Reduce sample size since writes involve I/O.
    group.sample_size(20);

    for &count in &[10, 100, 500] {
        let stubs = generate_test_stubs(count);
        let dir = tempfile::tempdir().expect("create tempdir");
        let cache = StubCache::new(dir.path());
        let (_jar_dir, jar_path) = generate_test_jar(count);

        group.bench_with_input(
            BenchmarkId::new("cache_write", count),
            &(&cache, &jar_path, &stubs),
            |b, (cache, path, stubs)| {
                b.iter(|| cache.put(std::hint::black_box(path), std::hint::black_box(stubs)));
            },
        );
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmarks: Index persistence (save/load roundtrip)
// ---------------------------------------------------------------------------

fn bench_index_persistence(c: &mut Criterion) {
    let mut group = c.benchmark_group("index_persistence");
    group.sample_size(20);

    let stubs = generate_test_stubs(5_000);
    let index = ClasspathIndex::build(stubs);

    let dir = tempfile::tempdir().expect("create tempdir");
    let index_path = dir.path().join("index.sqry");

    // Save benchmark.
    group.bench_function("save_5000", |b| {
        b.iter(|| index.save(std::hint::black_box(&index_path)));
    });

    // Save once so we can benchmark load.
    index.save(&index_path).expect("save index for load bench");

    // Load benchmark.
    group.bench_function("load_5000", |b| {
        b.iter(|| ClasspathIndex::load(std::hint::black_box(&index_path)));
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Criterion harness
// ---------------------------------------------------------------------------

criterion_group!(
    benches,
    bench_class_parsing,
    bench_generic_signatures,
    bench_jar_scanning,
    bench_index_build,
    bench_index_lookup,
    bench_stub_cache,
    bench_stub_cache_write,
    bench_index_persistence,
);
criterion_main!(benches);
