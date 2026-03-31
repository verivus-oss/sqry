use criterion::{Criterion, criterion_group, criterion_main};
use sqry_core::ast::IncrementalParser;
use std::hint::black_box;
use std::path::PathBuf;

// Mock plugin for benchmarking
struct BenchPlugin;

impl sqry_core::plugin::LanguagePlugin for BenchPlugin {
    fn metadata(&self) -> sqry_core::plugin::LanguageMetadata {
        sqry_core::plugin::LanguageMetadata {
            id: "rust",
            name: "Rust",
            version: "1.0.0",
            author: "Bench",
            description: "Benchmark plugin",
            tree_sitter_version: "0.24",
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["rs"]
    }

    fn language(&self) -> tree_sitter::Language {
        tree_sitter_rust::LANGUAGE.into()
    }

    fn parse_ast(
        &self,
        content: &[u8],
    ) -> Result<tree_sitter::Tree, sqry_core::plugin::error::ParseError> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&self.language())
            .map_err(|_| sqry_core::plugin::error::ParseError::TreeSitterFailed)?;
        parser
            .parse(content, None)
            .ok_or(sqry_core::plugin::error::ParseError::TreeSitterFailed)
    }

    fn extract_scopes(
        &self,
        _tree: &tree_sitter::Tree,
        _content: &[u8],
        _file_path: &std::path::Path,
    ) -> Result<Vec<sqry_core::ast::Scope>, sqry_core::plugin::error::ScopeError> {
        Ok(Vec::new())
    }
}

// Benchmark: Full parse vs Incremental parse
fn bench_full_vs_incremental(c: &mut Criterion) {
    let parser = IncrementalParser::with_default_capacity();
    let plugin = BenchPlugin;
    let path = PathBuf::from("/bench/file.rs");

    let original_content = b"fn main() {\n    let x = 1;\n    let y = 2;\n    println!(\"{}\", x + y);\n}\n\nfn helper() {\n    let a = 10;\n}\n";
    let modified_content = b"fn main() {\n    let x = 1;\n    let y = 3;\n    println!(\"{}\", x + y);\n}\n\nfn helper() {\n    let a = 10;\n}\n";

    // Warm up cache
    parser
        .parse(&plugin, &path, original_content, None)
        .unwrap();

    let mut group = c.benchmark_group("parse_comparison");

    group.bench_function("full_parse", |b| {
        b.iter(|| {
            parser.clear_cache();
            black_box(
                parser
                    .parse(&plugin, &path, modified_content, None)
                    .unwrap(),
            )
        });
    });

    group.bench_function("incremental_parse", |b| {
        b.iter(|| {
            // Re-warm cache before each iteration
            parser
                .parse(&plugin, &path, original_content, None)
                .unwrap();
            black_box(
                parser
                    .parse(&plugin, &path, modified_content, Some(original_content))
                    .unwrap(),
            )
        });
    });

    group.finish();
}

criterion_group!(benches, bench_full_vs_incremental);
criterion_main!(benches);
