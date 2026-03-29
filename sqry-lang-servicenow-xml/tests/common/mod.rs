#![allow(dead_code)]

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::build::staging::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_servicenow_xml::{ServiceNowXmlGraphBuilder, ServiceNowXmlPlugin};
use std::path::Path;

pub fn build_graph_from_xml(xml: &str) -> StagingGraph {
    let plugin = ServiceNowXmlPlugin::new();
    let tree = plugin.parse_ast(xml.as_bytes()).expect("parse_ast");
    let mut staging = StagingGraph::new();
    let builder = ServiceNowXmlGraphBuilder;
    builder
        .build_graph(&tree, xml.as_bytes(), Path::new("test.xml"), &mut staging)
        .expect("build_graph");
    staging
}

pub fn build_graph_from_file(path: &str) -> StagingGraph {
    let xml =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("Failed to read {path}: {e}"));
    let plugin = ServiceNowXmlPlugin::new();
    let tree = plugin.parse_ast(xml.as_bytes()).expect("parse_ast");
    let mut staging = StagingGraph::new();
    let builder = ServiceNowXmlGraphBuilder;
    builder
        .build_graph(&tree, xml.as_bytes(), Path::new(path), &mut staging)
        .expect("build_graph");
    staging
}

pub fn count_nodes_of_kind(staging: &StagingGraph, kind: NodeKind) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| matches!(op, StagingOp::AddNode { entry, .. } if entry.kind == kind))
        .count()
}

pub fn count_edges_of_kind(staging: &StagingGraph, check: impl Fn(&EdgeKind) -> bool) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| matches!(op, StagingOp::AddEdge { kind, .. } if check(kind)))
        .count()
}
