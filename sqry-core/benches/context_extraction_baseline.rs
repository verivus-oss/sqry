use criterion::{Criterion, criterion_group, criterion_main};
use sqry_core::ast::ContextExtractor;
use std::hint::black_box;
use std::path::PathBuf;
use tempfile::TempDir;

fn create_test_corpus() -> (TempDir, Vec<PathBuf>) {
    let temp_dir = tempfile::tempdir().unwrap();
    let mut files = Vec::new();

    // Create 10 test Rust files with varying complexity
    for i in 0..10 {
        let file_path = temp_dir.path().join(format!("test_{i}.rs"));
        let content = format!(
            r#"
// File {i}
pub fn top_level_function_{i}() {{
    println!("Top level");
}}

pub struct TestStruct{i} {{
    field: i32,
}}

impl TestStruct{i} {{
    pub fn method_{i}(&self) -> i32 {{
        self.field
  }}

    pub fn nested_method_{i}(&self) {{
        let closure = || {{
            println!("Nested closure");
      }};
        closure();
  }}
}}

pub mod nested_module_{i} {{
    pub fn inner_function_{i}() {{
        let x = 42;
        if x > 0 {{
            println!("Positive");
      }}
  }}

    pub struct InnerStruct{i} {{
        data: String,
  }}

    impl InnerStruct{i} {{
        pub fn new() -> Self {{
            Self {{ data: String::new() }}
      }}
  }}
}}

#[cfg(test)]
mod tests_{i} {{
    use super::*;

    #[test]
    fn test_function_{i}() {{
        assert_eq!(2 + 2, 4);
  }}
}}
"#
        );
        std::fs::write(&file_path, content).unwrap();
        files.push(file_path);
    }

    (temp_dir, files)
}

fn benchmark_context_extraction(c: &mut Criterion) {
    let (_temp_dir, files) = create_test_corpus();
    let extractor = ContextExtractor::new();

    c.bench_function("context_extraction_baseline_per_file", |b| {
        b.iter(|| {
            let file = &files[0];
            let _ = extractor.extract_from_file(black_box(file));
        });
    });

    c.bench_function("context_extraction_baseline_10_files", |b| {
        b.iter(|| {
            for file in &files {
                let _ = extractor.extract_from_file(black_box(file));
            }
        });
    });
}

fn benchmark_parsing_overhead(c: &mut Criterion) {
    let (_temp_dir, files) = create_test_corpus();
    let extractor = ContextExtractor::new();

    // This benchmark measures the double-parsing issue (H4)
    // by timing context extraction which currently parses twice
    c.bench_function("parsing_overhead_current", |b| {
        b.iter(|| {
            let file = &files[0];
            // Current implementation parses twice:
            // 1. Once in extract_symbols()
            // 2. Once in extract_from_file() for context
            let _ = extractor.extract_from_file(black_box(file));
        });
    });
}

criterion_group!(
    benches,
    benchmark_context_extraction,
    benchmark_parsing_overhead
);
criterion_main!(benches);
