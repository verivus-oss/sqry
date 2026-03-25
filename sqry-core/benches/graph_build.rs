use std::path::Path;

use criterion::{Criterion, criterion_group, criterion_main};
use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::StagingGraph;
use sqry_lang_cpp::relations::CppGraphBuilder;
use std::hint::black_box;
use tree_sitter::Parser;

fn cpp_graph_build(c: &mut Criterion) {
    let source = r"
namespace foo {
    int helper(int v) {
        return v * 2;
  }

    int chain(int v) {
        for (int i = 0; i < 10; ++i) {
            v = helper(v);
      }
        return v;
  }

    int entry() {
        auto lam = [](int x) { return helper(x); };
        return lam(chain(3));
  }
}
";

    let mut parser = Parser::new();
    let language = tree_sitter_cpp::LANGUAGE;
    parser
        .set_language(&language.into())
        .expect("load C++ grammar");
    let tree = parser.parse(source, None).expect("parse C++");
    let builder = CppGraphBuilder;
    let file = Path::new("bench.cpp");

    c.bench_function("cpp_graph_build", |b| {
        b.iter(|| {
            let mut staging = StagingGraph::new();
            builder
                .build_graph(&tree, black_box(source.as_bytes()), file, &mut staging)
                .expect("build graph");
        });
    });
}

criterion_group!(graph_benches, cpp_graph_build);
criterion_main!(graph_benches);
