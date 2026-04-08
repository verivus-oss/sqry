//! Export edge tests for Lua `GraphBuilder`
//!
//! These tests verify that the Lua `GraphBuilder` correctly creates Export edges
//! for various Lua export patterns.

#[cfg(test)]
mod tests {
    use crate::LuaPlugin;
    use sqry_core::graph::GraphBuilder;
    use sqry_core::graph::unified::StagingGraph;
    use sqry_core::graph::unified::build::StagingOp;
    use sqry_core::graph::unified::edge::EdgeKind as UnifiedEdgeKind;
    use sqry_core::graph::unified::node::NodeId;
    use sqry_core::plugin::LanguagePlugin;
    use std::path::PathBuf;
    use tree_sitter::Tree;

    use super::super::graph_builder::LuaGraphBuilder;

    fn parse_lua(source: &str) -> Tree {
        let plugin = LuaPlugin::default();
        plugin.parse_ast(source.as_bytes()).unwrap()
    }

    /// Helper to extract Export edges from staging operations
    fn extract_export_edges(staging: &StagingGraph) -> Vec<&UnifiedEdgeKind> {
        staging
            .operations()
            .iter()
            .filter_map(|op| {
                if let StagingOp::AddEdge { kind, .. } = op
                    && matches!(kind, UnifiedEdgeKind::Exports { .. })
                {
                    return Some(kind);
                }
                None
            })
            .collect()
    }

    #[test]
    fn test_export_function_declaration() {
        let source = r"
            local M = {}

            function M.public_function(x)
                return x * 2
            end

            return M
        ";

        let tree = parse_lua(source);
        let mut staging = StagingGraph::new();
        let builder = LuaGraphBuilder::default();
        let file = PathBuf::from("test.lua");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let export_edges = extract_export_edges(&staging);
        assert!(
            !export_edges.is_empty(),
            "Expected at least one export edge for M.public_function"
        );

        // Verify it's a Direct export
        let edge = export_edges[0];
        if let UnifiedEdgeKind::Exports { kind, alias } = edge {
            use sqry_core::graph::unified::edge::ExportKind;
            assert_eq!(*kind, ExportKind::Direct);
            assert!(alias.is_none());
        } else {
            panic!("Expected Exports edge kind");
        }
    }

    #[test]
    fn test_export_assignment() {
        let source = r"
            local M = {}

            M.fetch = function(url)
                return {}
            end

            return M
        ";

        let tree = parse_lua(source);
        let mut staging = StagingGraph::new();
        let builder = LuaGraphBuilder::default();
        let file = PathBuf::from("test.lua");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let export_edges = extract_export_edges(&staging);
        assert!(
            !export_edges.is_empty(),
            "Expected export edge for M.fetch assignment"
        );
    }

    #[test]
    fn test_export_colon_method() {
        let source = r"
            local Class = {}

            function Class:method_one()
                return self
            end

            return Class
        ";

        let tree = parse_lua(source);
        let mut staging = StagingGraph::new();
        let builder = LuaGraphBuilder::default();
        let file = PathBuf::from("test.lua");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let export_edges = extract_export_edges(&staging);
        assert!(
            !export_edges.is_empty(),
            "Expected export edge for colon method"
        );
    }

    #[test]
    fn test_export_return_table_direct_reference() {
        let source = r#"
            function global_func()
                return "global"
            end

            return {
                global_func = global_func
            }
        "#;

        let tree = parse_lua(source);
        let mut staging = StagingGraph::new();
        let builder = LuaGraphBuilder::default();
        let file = PathBuf::from("test.lua");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let export_edges = extract_export_edges(&staging);
        assert!(
            !export_edges.is_empty(),
            "Expected export edge for return table entry"
        );
    }

    #[test]
    fn test_export_return_table_module_reference() {
        let source = r#"
            function Module.namespaced_func()
                return "namespaced"
            end

            return {
                namespaced = Module.namespaced_func
            }
        "#;

        let tree = parse_lua(source);
        let mut staging = StagingGraph::new();
        let builder = LuaGraphBuilder::default();
        let file = PathBuf::from("test.lua");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let export_edges = extract_export_edges(&staging);
        assert!(
            export_edges.len() >= 2,
            "Expected export edges for both function declaration and return table"
        );
    }

    #[test]
    fn test_export_no_local_functions() {
        let source = r"
            local M = {}

            local function private_helper()
                return 42
            end

            function M.public_function(x)
                return x * 2
            end

            return M
        ";

        let tree = parse_lua(source);
        let mut staging = StagingGraph::new();
        let builder = LuaGraphBuilder::default();
        let file = PathBuf::from("test.lua");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        // Extract export edge target NodeIds
        let export_target_ids: Vec<NodeId> = staging
            .operations()
            .iter()
            .filter_map(|op| {
                if let StagingOp::AddEdge { target, kind, .. } = op
                    && matches!(kind, UnifiedEdgeKind::Exports { .. })
                {
                    return Some(*target);
                }
                None
            })
            .collect();

        // Should have at least one export
        assert!(
            !export_target_ids.is_empty(),
            "Should have at least one export"
        );

        // Find the node names that correspond to the export targets
        let exported_names: Vec<String> = staging
            .operations()
            .iter()
            .filter_map(|op| {
                if let StagingOp::AddNode { entry, expected_id } = op
                    && expected_id.is_some()
                    && export_target_ids.contains(&expected_id.unwrap())
                {
                    return Some(entry.name.to_string());
                }
                None
            })
            .collect();

        // CRITICAL: private_helper should NOT be exported
        for name in &exported_names {
            assert!(
                !name.contains("private_helper"),
                "private_helper should NOT be exported, but found export target: {name}"
            );
        }
    }

    #[test]
    fn test_export_bracket_assignment() {
        let source = r#"
            local Handlers = {}

            Handlers["on-click"] = function(event)
                return true
            end

            return Handlers
        "#;

        let tree = parse_lua(source);
        let mut staging = StagingGraph::new();
        let builder = LuaGraphBuilder::default();
        let file = PathBuf::from("test.lua");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let export_edges = extract_export_edges(&staging);
        assert!(
            !export_edges.is_empty(),
            "Expected export edge for bracket assignment"
        );
    }

    #[test]
    fn test_export_return_table_inline_function() {
        let source = r#"
            return {
                inline_func = function()
                    return "inline"
                end
            }
        "#;

        let tree = parse_lua(source);
        let mut staging = StagingGraph::new();
        let builder = LuaGraphBuilder::default();
        let file = PathBuf::from("test.lua");

        builder
            .build_graph(&tree, source.as_bytes(), &file, &mut staging)
            .unwrap();

        let export_edges = extract_export_edges(&staging);
        assert!(
            !export_edges.is_empty(),
            "Expected export edge for inline function in return table"
        );
    }
}
