use criterion::{Criterion, criterion_group, criterion_main};
use sqry_core::search::fuzzy::{CandidateGenerator, FuzzyConfig};
use sqry_core::search::trigram::TrigramIndex;
use std::hint::black_box;
use std::sync::Arc;

fn create_test_index(symbol_count: usize) -> TrigramIndex {
    let mut index = TrigramIndex::new();

    // Sample symbol names from TypeScript compiler codebase
    let sample_names = vec![
        "parseSourceFile",
        "createSourceFile",
        "visitNode",
        "bindSourceFile",
        "checkSourceFile",
        "emitSourceFile",
        "forEachChild",
        "getNodeId",
        "getSourceFileOfNode",
        "isDeclaration",
        "createScanner",
        "scan",
        "getTokenText",
        "getTokenPos",
        "getTokenEnd",
        "skipTrivia",
        "isIdentifier",
        "isLiteralKind",
        "isTemplateLiteralKind",
        "isModifierKind",
        "createNodeArray",
        "createLiteral",
        "createIdentifier",
        "updateSourceFile",
        "visitEachChild",
        "visitLexicalEnvironment",
        "chainBundle",
        "transformNodes",
        "getTransformers",
        "noEmitSubstitution",
        "emitHelpers",
        "addEmitHelper",
        "createPrinter",
        "printNode",
        "printList",
        "printFile",
        "writeFile",
        "getPreEmitDiagnostics",
        "createProgram",
        "createCompilerHost",
        "parseCommandLine",
        "readConfigFile",
        "getParsedCommandLine",
        "createWatchCompilerHost",
        "resolveModuleName",
        "resolveTypeReferenceDirective",
        "getAutomaticTypeDirectiveNames",
        "createModuleResolutionCache",
        "getResolvedModule",
        "getResolvedTypeReferenceDirective",
        "hasExtension",
        "removeFileExtension",
        "getBaseFileName",
        "getDirectoryPath",
        "getRootLength",
        "normalizePath",
    ];

    // Repeat names to create larger index
    for i in 0..symbol_count {
        let name = &sample_names[i % sample_names.len()];
        let full_name = if i < sample_names.len() {
            (*name).to_string()
        } else {
            format!("{}_{}", name, i / sample_names.len())
        };
        index.add_symbol(i, &full_name);
    }

    index
}

fn benchmark_candidate_generation_jaccard(c: &mut Criterion) {
    // Set environment variable to enable Jaccard
    unsafe {
        std::env::set_var("SQRY_FUZZY_USE_JACCARD", "1");
    }

    let index = Arc::new(create_test_index(10000));
    let config = FuzzyConfig {
        max_candidates: 1000,
        min_similarity: 0.1,
    };
    let generator = CandidateGenerator::with_config(index, config);

    let queries = vec!["parse", "create", "visit", "emit", "resolve", "get"];

    c.bench_function("candidate_gen_jaccard_10k", |b| {
        b.iter(|| {
            for query in &queries {
                let candidates = generator.generate(black_box(query));
                black_box(candidates);
            }
        });
    });
}

fn benchmark_candidate_generation_ratio(c: &mut Criterion) {
    // Set environment variable to disable Jaccard
    unsafe {
        std::env::set_var("SQRY_FUZZY_USE_JACCARD", "0");
    }

    let index = Arc::new(create_test_index(10000));
    let config = FuzzyConfig {
        max_candidates: 1000,
        min_similarity: 0.1,
    };
    let generator = CandidateGenerator::with_config(index, config);

    let queries = vec!["parse", "create", "visit", "emit", "resolve", "get"];

    c.bench_function("candidate_gen_ratio_10k", |b| {
        b.iter(|| {
            for query in &queries {
                let candidates = generator.generate(black_box(query));
                black_box(candidates);
            }
        });
    });
}

fn benchmark_candidate_count_comparison(_c: &mut Criterion) {
    let index = Arc::new(create_test_index(10000));
    let config = FuzzyConfig {
        max_candidates: 1000,
        min_similarity: 0.1,
    };

    let queries = vec!["parse", "create", "visit", "emit", "resolve", "get"];

    // Jaccard mode
    unsafe {
        std::env::set_var("SQRY_FUZZY_USE_JACCARD", "1");
    }
    let generator_jaccard = CandidateGenerator::with_config(index.clone(), config.clone());
    let mut jaccard_counts = Vec::new();
    for query in &queries {
        let candidates = generator_jaccard.generate(query);
        jaccard_counts.push(candidates.len());
    }

    // Ratio mode
    unsafe {
        std::env::set_var("SQRY_FUZZY_USE_JACCARD", "0");
    }
    let generator_ratio = CandidateGenerator::with_config(index.clone(), config);
    let mut ratio_counts = Vec::new();
    for query in &queries {
        let candidates = generator_ratio.generate(query);
        ratio_counts.push(candidates.len());
    }

    println!("\n=== Candidate Count Comparison ===");
    for (i, query) in queries.iter().enumerate() {
        let ratio_count = u32::try_from(ratio_counts[i]).expect("ratio count fits u32");
        let jaccard_count = u32::try_from(jaccard_counts[i]).expect("jaccard count fits u32");
        let reduction = if ratio_count > 0 {
            let ratio_f64 = f64::from(ratio_count);
            let reduction_count = ratio_count.saturating_sub(jaccard_count);
            let reduction_f64 = f64::from(reduction_count);
            100.0 * reduction_f64 / ratio_f64
        } else {
            0.0
        };
        println!(
            "Query '{}': Jaccard={}, Ratio={}, Reduction={:.1}%",
            query, jaccard_counts[i], ratio_counts[i], reduction
        );
    }
}

criterion_group!(
    benches,
    benchmark_candidate_generation_jaccard,
    benchmark_candidate_generation_ratio,
    benchmark_candidate_count_comparison
);
criterion_main!(benches);
