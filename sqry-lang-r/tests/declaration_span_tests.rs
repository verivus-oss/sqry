//! Regression coverage for R declaration lines.
//!
//! `span_from_points` pre-incremented the row into a 0-based `Position`, and the
//! staging layer increments again, so every R declaration reported one line too
//! low. PR #742 noticed the symptom ("R has a separate off-by-one") and deferred
//! it; two reviewers of PR #746 measured the cause. The fixture pads the top so
//! an off-by-one is unambiguous.

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use std::collections::HashMap;
use std::path::Path;

#[test]
fn declaration_reports_its_real_line_not_one_line_low() {
    let source = "# pad\n# pad\n# pad\nmyfunc <- function(x) {\n  x + 1\n}\n";
    let expected = u32::try_from(
        source[..source.find("myfunc").expect("in fixture")]
            .matches('\n')
            .count()
            + 1,
    )
    .expect("fits");

    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_r::LANGUAGE.into())
        .expect("grammar");
    let tree = parser.parse(source, None).expect("parse");

    let mut staging = StagingGraph::new();
    sqry_lang_r::relations::RGraphBuilder::default()
        .build_graph(&tree, source.as_bytes(), Path::new("m.R"), &mut staging)
        .expect("build graph");

    let strings: HashMap<u32, String> = staging
        .operations()
        .iter()
        .filter_map(|op| match op {
            StagingOp::InternString { local_id, value } => Some((local_id.index(), value.clone())),
            _ => None,
        })
        .collect();

    let found = staging
        .operations()
        .iter()
        .find_map(|op| match op {
            StagingOp::AddNode { entry, .. } => {
                let id = entry.qualified_name.unwrap_or(entry.name).index();
                (strings.get(&id).map(String::as_str) == Some("myfunc")).then_some(entry.start_line)
            }
            _ => None,
        })
        .expect("`myfunc` must be staged");

    assert_eq!(
        found, expected,
        "`myfunc` is declared on line {expected}; reporting {found} means the 1-based \
         conversion was applied twice, once in the plugin and once in staging"
    );
}
