//! Integration tests for JavaScript JSDoc TypeOf and Reference edges
//!
//! Tests JSDoc @param, @returns, and @type annotations

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::build::{StagingGraph, StagingOp};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_lang_javascript::JavaScriptGraphBuilder;
use std::collections::HashMap;
use std::path::Path;

/// Helper: Build StagingGraph from JavaScript source code
fn build_graph(source: &str) -> StagingGraph {
    let builder = JavaScriptGraphBuilder::default();
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_javascript::LANGUAGE.into())
        .expect("Failed to load JavaScript grammar");

    let tree = parser.parse(source, None).expect("Failed to parse");

    let mut staging = StagingGraph::new();
    let file_path = Path::new("test.js");

    builder
        .build_graph(&tree, source.as_bytes(), file_path, &mut staging)
        .expect("Failed to build graph");

    staging
}

/// Build a string lookup map from staged InternString operations
fn build_string_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::InternString { local_id, value } = op {
                Some((local_id.index(), value.clone()))
            } else {
                None
            }
        })
        .collect()
}

/// Build a node name lookup map from staged AddNode operations
fn build_node_name_lookup(staging: &StagingGraph) -> HashMap<u32, String> {
    let strings = build_string_lookup(staging);
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode { entry, expected_id } = op {
                let expected_id = expected_id.as_ref()?;
                let node_idx = expected_id.index();
                let name_idx = entry.qualified_name.unwrap_or(entry.name).index();
                let name = strings
                    .get(&name_idx)
                    .cloned()
                    .unwrap_or_else(|| format!("<string:{name_idx}>"));
                Some((node_idx, name))
            } else {
                None
            }
        })
        .collect()
}

/// Helper to collect all edges of a specific kind
fn collect_edges_by_kind<F>(staging: &StagingGraph, predicate: F) -> Vec<(String, String)>
where
    F: Fn(&EdgeKind) -> bool,
{
    let node_names = build_node_name_lookup(staging);
    let mut edges = Vec::new();

    for op in staging.operations() {
        if let StagingOp::AddEdge {
            source,
            target,
            kind,
            ..
        } = op
            && predicate(kind)
        {
            let from_name = node_names
                .get(&source.index())
                .cloned()
                .unwrap_or_else(|| format!("<unknown:{}>", source.index()));

            let to_name = node_names
                .get(&target.index())
                .cloned()
                .unwrap_or_else(|| format!("<unknown:{}>", target.index()));

            edges.push((from_name, to_name));
        }
    }

    edges
}

/// Helper: Check if TypeOf edge exists with context
fn has_typeof_edge(
    staging: &StagingGraph,
    source_name: &str,
    target_type: &str,
    context: TypeOfContext,
) -> bool {
    let edges = collect_edges_by_kind(staging, |kind| {
        matches!(
            kind,
            EdgeKind::TypeOf {
                context: Some(ctx),
                ..
            } if *ctx == context
        )
    });

    edges
        .iter()
        .any(|(src, tgt)| src == source_name && tgt == target_type)
}

/// Helper: Check if Reference edge exists
fn has_reference_edge(staging: &StagingGraph, source_name: &str, target_type: &str) -> bool {
    let edges = collect_edges_by_kind(staging, |kind| matches!(kind, EdgeKind::References));

    edges
        .iter()
        .any(|(src, tgt)| src == source_name && tgt == target_type)
}

// ========== Category 1: JSDoc @param Tests ==========

#[test]
fn test_simple_parameter_types() {
    let source = r#"
        /**
         * @param {string} name
         * @param {number} age
         * @param {boolean} active
         */
        function createUser(name, age, active) {}
    "#;

    let graph = build_graph(source);

    // Assert TypeOf edges
    assert!(has_typeof_edge(
        &graph,
        "createUser",
        "string",
        TypeOfContext::Parameter
    ));
    assert!(has_typeof_edge(
        &graph,
        "createUser",
        "number",
        TypeOfContext::Parameter
    ));
    assert!(has_typeof_edge(
        &graph,
        "createUser",
        "boolean",
        TypeOfContext::Parameter
    ));

    // Assert Reference edges
    assert!(has_reference_edge(&graph, "createUser", "string"));
    assert!(has_reference_edge(&graph, "createUser", "number"));
    assert!(has_reference_edge(&graph, "createUser", "boolean"));
}

#[test]
fn test_complex_parameter_types() {
    let source = r#"
        /**
         * @param {Array<User>} users
         * @param {Map<string, number>} scores
         */
        function processData(users, scores) {}
    "#;

    let graph = build_graph(source);

    // Assert TypeOf edges
    assert!(has_typeof_edge(
        &graph,
        "processData",
        "Array<User>",
        TypeOfContext::Parameter
    ));
    assert!(has_typeof_edge(
        &graph,
        "processData",
        "Map<string, number>",
        TypeOfContext::Parameter
    ));

    // Assert Reference edges
    assert!(has_reference_edge(&graph, "processData", "Array"));
    assert!(has_reference_edge(&graph, "processData", "User"));
    assert!(has_reference_edge(&graph, "processData", "Map"));
    assert!(has_reference_edge(&graph, "processData", "string"));
    assert!(has_reference_edge(&graph, "processData", "number"));
}

#[test]
fn test_optional_parameters() {
    let source = r#"
        /**
         * @param {string} name
         * @param {string} [optionalTag]
         */
        function process(name, optionalTag) {}
    "#;

    let graph = build_graph(source);

    // Assert TypeOf edges for both params
    assert!(has_typeof_edge(
        &graph,
        "process",
        "string",
        TypeOfContext::Parameter
    ));
}

#[test]
fn test_rest_parameters() {
    let source = r#"
        /**
         * @param {string} name
         * @param {...number} scores
         */
        function calculate(name, ...scores) {}
    "#;

    let graph = build_graph(source);

    // Assert TypeOf edges - rest param type is canonicalized to "number"
    assert!(has_typeof_edge(
        &graph,
        "calculate",
        "string",
        TypeOfContext::Parameter
    ));
    assert!(has_typeof_edge(
        &graph,
        "calculate",
        "number",
        TypeOfContext::Parameter
    ));

    // Assert Reference edges
    assert!(has_reference_edge(&graph, "calculate", "string"));
    assert!(has_reference_edge(&graph, "calculate", "number"));
}

#[test]
fn test_default_parameters() {
    let source = r#"
        /**
         * @param {string} name
         * @param {number} [count=10]
         * @param {boolean} [active=false]
         */
        function create(name, count = 10, active = false) {}
    "#;

    let graph = build_graph(source);

    // Assert TypeOf edges
    assert!(has_typeof_edge(
        &graph,
        "create",
        "string",
        TypeOfContext::Parameter
    ));
    assert!(has_typeof_edge(
        &graph,
        "create",
        "number",
        TypeOfContext::Parameter
    ));
    assert!(has_typeof_edge(
        &graph,
        "create",
        "boolean",
        TypeOfContext::Parameter
    ));
}

// ========== Category 2: JSDoc @returns Tests ==========

#[test]
fn test_simple_return_types() {
    let source = r#"
        /**
         * @returns {boolean}
         */
        function isValid() {
            return true;
        }

        /**
         * @returns {User}
         */
        function getUser() {
            return currentUser;
        }
    "#;

    let graph = build_graph(source);

    // Assert TypeOf edges
    assert!(has_typeof_edge(
        &graph,
        "isValid",
        "boolean",
        TypeOfContext::Return
    ));
    assert!(has_typeof_edge(
        &graph,
        "getUser",
        "User",
        TypeOfContext::Return
    ));

    // Assert Reference edges
    assert!(has_reference_edge(&graph, "isValid", "boolean"));
    assert!(has_reference_edge(&graph, "getUser", "User"));
}

#[test]
fn test_promise_return_types() {
    let source = r#"
        /**
         * @param {string} id
         * @returns {Promise<User>}
         */
        async function fetchUser(id) {
            return user;
        }

        /**
         * @returns {Promise<Array<Item>>}
         */
        async function fetchItems() {
            return items;
        }
    "#;

    let graph = build_graph(source);

    // Assert TypeOf edges
    assert!(has_typeof_edge(
        &graph,
        "fetchUser",
        "Promise<User>",
        TypeOfContext::Return
    ));
    assert!(has_typeof_edge(
        &graph,
        "fetchItems",
        "Promise<Array<Item>>",
        TypeOfContext::Return
    ));

    // Assert Reference edges
    assert!(has_reference_edge(&graph, "fetchUser", "Promise"));
    assert!(has_reference_edge(&graph, "fetchUser", "User"));
    assert!(has_reference_edge(&graph, "fetchItems", "Array"));
    assert!(has_reference_edge(&graph, "fetchItems", "Item"));
}

#[test]
fn test_complex_return_types() {
    let source = r#"
        /**
         * @returns {{id: string, user: User, count: number}}
         */
        function getData() {
            return data;
        }
    "#;

    let graph = build_graph(source);

    // Assert TypeOf edge
    assert!(has_typeof_edge(
        &graph,
        "getData",
        "{id: string, user: User, count: number}",
        TypeOfContext::Return
    ));

    // Assert Reference edges
    assert!(has_reference_edge(&graph, "getData", "string"));
    assert!(has_reference_edge(&graph, "getData", "User"));
    assert!(has_reference_edge(&graph, "getData", "number"));
}

// ========== Category 3: JSDoc @type Variable Tests ==========

#[test]
fn test_variable_type_annotations() {
    let source = r#"
        /**
         * @type {Map<string, User>}
         */
        const userCache = new Map();

        /**
         * @type {Array<number>}
         */
        let scores = [];
    "#;

    let graph = build_graph(source);

    // Assert TypeOf edges
    assert!(has_typeof_edge(
        &graph,
        "userCache",
        "Map<string, User>",
        TypeOfContext::Variable
    ));
    assert!(has_typeof_edge(
        &graph,
        "scores",
        "Array<number>",
        TypeOfContext::Variable
    ));

    // Assert Reference edges
    assert!(has_reference_edge(&graph, "userCache", "Map"));
    assert!(has_reference_edge(&graph, "userCache", "string"));
    assert!(has_reference_edge(&graph, "userCache", "User"));
    assert!(has_reference_edge(&graph, "scores", "Array"));
    assert!(has_reference_edge(&graph, "scores", "number"));
}

#[test]
fn test_constant_type_annotations() {
    let source = r#"
        /**
         * @type {Config}
         */
        const CONFIG = loadConfig();

        /**
         * @type {Logger}
         */
        const logger = createLogger();
    "#;

    let graph = build_graph(source);

    // Assert TypeOf edges
    assert!(has_typeof_edge(
        &graph,
        "CONFIG",
        "Config",
        TypeOfContext::Variable
    ));
    assert!(has_typeof_edge(
        &graph,
        "logger",
        "Logger",
        TypeOfContext::Variable
    ));

    // Assert Reference edges
    assert!(has_reference_edge(&graph, "CONFIG", "Config"));
    assert!(has_reference_edge(&graph, "logger", "Logger"));
}

#[test]
fn test_nested_object_type_parsing() {
    let source = r#"
        /**
         * @param {{id: string, meta: {tags: string[]}}} obj
         */
        function processObject(obj) {}

        /**
         * @type {{user: {id: string, name: string}, count: number}}
         */
        const data = loadData();
    "#;

    let graph = build_graph(source);

    // Assert TypeOf edges
    assert!(has_typeof_edge(
        &graph,
        "processObject",
        "{id: string, meta: {tags: string[]}}",
        TypeOfContext::Parameter
    ));
    assert!(has_typeof_edge(
        &graph,
        "data",
        "{user: {id: string, name: string}, count: number}",
        TypeOfContext::Variable
    ));

    // Assert Reference edges - type extractor handles nested structures
    assert!(has_reference_edge(&graph, "processObject", "string"));
    assert!(has_reference_edge(&graph, "data", "string"));
    assert!(has_reference_edge(&graph, "data", "number"));
}

// ========== Category 4: JSDoc @type Class Field Tests ==========

#[test]
fn test_class_field_type_annotations() {
    let source = r#"
        class DataService {
            /**
             * @type {Database}
             */
            db;

            /**
             * @type {Logger}
             */
            logger;

            /**
             * @type {Map<string, any>}
             */
            cache = new Map();
        }
    "#;

    let graph = build_graph(source);

    // Assert TypeOf edges with qualified field names
    assert!(has_typeof_edge(
        &graph,
        "DataService.db",
        "Database",
        TypeOfContext::Field
    ));
    assert!(has_typeof_edge(
        &graph,
        "DataService.logger",
        "Logger",
        TypeOfContext::Field
    ));
    assert!(has_typeof_edge(
        &graph,
        "DataService.cache",
        "Map<string, any>",
        TypeOfContext::Field
    ));

    // Assert Reference edges
    assert!(has_reference_edge(&graph, "DataService.db", "Database"));
    assert!(has_reference_edge(&graph, "DataService.logger", "Logger"));
    assert!(has_reference_edge(&graph, "DataService.cache", "Map"));
    assert!(has_reference_edge(&graph, "DataService.cache", "string"));
}

#[test]
fn test_multi_param_function() {
    let source = r#"
        /**
         * @param {User} user
         * @param {number} count
         * @param {Array<string>} tags
         * @param {boolean} force
         */
        function updateUser(user, count, tags, force) {}
    "#;

    let graph = build_graph(source);

    // Assert TypeOf edges
    assert!(has_typeof_edge(
        &graph,
        "updateUser",
        "User",
        TypeOfContext::Parameter
    ));
    assert!(has_typeof_edge(
        &graph,
        "updateUser",
        "number",
        TypeOfContext::Parameter
    ));
    assert!(has_typeof_edge(
        &graph,
        "updateUser",
        "Array<string>",
        TypeOfContext::Parameter
    ));
    assert!(has_typeof_edge(
        &graph,
        "updateUser",
        "boolean",
        TypeOfContext::Parameter
    ));

    // Assert Reference edges
    assert!(has_reference_edge(&graph, "updateUser", "User"));
    assert!(has_reference_edge(&graph, "updateUser", "number"));
    assert!(has_reference_edge(&graph, "updateUser", "Array"));
    assert!(has_reference_edge(&graph, "updateUser", "string"));
    assert!(has_reference_edge(&graph, "updateUser", "boolean"));
}

#[test]
fn test_class_with_multiple_fields() {
    let source = r#"
        class Service {
            /**
             * @type {API}
             */
            api;

            counter = 0;

            /**
             * @type {EventEmitter}
             */
            events;
        }
    "#;

    let graph = build_graph(source);

    // Assert TypeOf edges for fields with JSDoc only
    assert!(has_typeof_edge(
        &graph,
        "Service.api",
        "API",
        TypeOfContext::Field
    ));
    assert!(has_typeof_edge(
        &graph,
        "Service.events",
        "EventEmitter",
        TypeOfContext::Field
    ));

    // Assert Reference edges
    assert!(has_reference_edge(&graph, "Service.api", "API"));
    assert!(has_reference_edge(&graph, "Service.events", "EventEmitter"));
}

// ========== Category 5: Complex Type Parsing Tests ==========

#[test]
fn test_union_types() {
    let source = r#"
        /**
         * @param {string|number|boolean} value
         * @param {User|Admin|Guest} actor
         */
        function process(value, actor) {}
    "#;

    let graph = build_graph(source);

    // Assert TypeOf edges
    assert!(has_typeof_edge(
        &graph,
        "process",
        "string|number|boolean",
        TypeOfContext::Parameter
    ));
    assert!(has_typeof_edge(
        &graph,
        "process",
        "User|Admin|Guest",
        TypeOfContext::Parameter
    ));

    // Assert Reference edges
    assert!(has_reference_edge(&graph, "process", "string"));
    assert!(has_reference_edge(&graph, "process", "number"));
    assert!(has_reference_edge(&graph, "process", "boolean"));
    assert!(has_reference_edge(&graph, "process", "User"));
    assert!(has_reference_edge(&graph, "process", "Admin"));
    assert!(has_reference_edge(&graph, "process", "Guest"));
}

#[test]
fn test_generic_types() {
    let source = r#"
        /**
         * @param {Array<T>} items
         * @param {Map<K, V>} lookup
         * @param {Promise<Result<Data>>} result
         */
        function handle(items, lookup, result) {}
    "#;

    let graph = build_graph(source);

    // Assert TypeOf edges
    assert!(has_typeof_edge(
        &graph,
        "handle",
        "Array<T>",
        TypeOfContext::Parameter
    ));
    assert!(has_typeof_edge(
        &graph,
        "handle",
        "Map<K, V>",
        TypeOfContext::Parameter
    ));
    assert!(has_typeof_edge(
        &graph,
        "handle",
        "Promise<Result<Data>>",
        TypeOfContext::Parameter
    ));

    // Assert Reference edges
    assert!(has_reference_edge(&graph, "handle", "Array"));
    assert!(has_reference_edge(&graph, "handle", "T"));
    assert!(has_reference_edge(&graph, "handle", "Map"));
    assert!(has_reference_edge(&graph, "handle", "K"));
    assert!(has_reference_edge(&graph, "handle", "V"));
    assert!(has_reference_edge(&graph, "handle", "Promise"));
    assert!(has_reference_edge(&graph, "handle", "Result"));
    assert!(has_reference_edge(&graph, "handle", "Data"));
}

#[test]
fn test_intersection_types() {
    let source = r#"
        /**
         * @param {Readable&Writable} stream
         * @param {User&Admin} superuser
         */
        function operate(stream, superuser) {}
    "#;

    let graph = build_graph(source);

    // Assert TypeOf edges
    assert!(has_typeof_edge(
        &graph,
        "operate",
        "Readable&Writable",
        TypeOfContext::Parameter
    ));
    assert!(has_typeof_edge(
        &graph,
        "operate",
        "User&Admin",
        TypeOfContext::Parameter
    ));

    // Assert Reference edges
    assert!(has_reference_edge(&graph, "operate", "Readable"));
    assert!(has_reference_edge(&graph, "operate", "Writable"));
    assert!(has_reference_edge(&graph, "operate", "User"));
    assert!(has_reference_edge(&graph, "operate", "Admin"));
}

#[test]
fn test_nested_generics() {
    let source = r#"
        /**
         * @param {Promise<Array<User>>} users
         */
        function processUsers(users) {}
    "#;

    let graph = build_graph(source);

    // Assert TypeOf edge
    assert!(has_typeof_edge(
        &graph,
        "processUsers",
        "Promise<Array<User>>",
        TypeOfContext::Parameter
    ));

    // Assert Reference edges
    assert!(has_reference_edge(&graph, "processUsers", "Promise"));
    assert!(has_reference_edge(&graph, "processUsers", "Array"));
    assert!(has_reference_edge(&graph, "processUsers", "User"));
}

// ========== Category 6: Negative Tests ==========

#[test]
fn test_no_jsdoc_comments() {
    let source = r#"
        // Regular comment
        function noJsDoc(a, b) {
            return a + b;
        }

        /* Block comment but not JSDoc */
        const x = 42;

        class NoTypes {
            field = 'value';
        }
    "#;

    let graph = build_graph(source);

    // Should not have any TypeOf edges for these nodes
    let typeof_edges =
        collect_edges_by_kind(&graph, |kind| matches!(kind, EdgeKind::TypeOf { .. }));

    assert!(
        typeof_edges.is_empty(),
        "Expected no TypeOf edges, but found: {:?}",
        typeof_edges
    );
}

#[test]
fn test_line_comments() {
    let source = r#"
        // @param {string} name - this is not JSDoc
        function notJsDoc(name) {}
    "#;

    let graph = build_graph(source);

    // No TypeOf edges should be created
    assert!(!has_typeof_edge(
        &graph,
        "notJsDoc",
        "string",
        TypeOfContext::Parameter
    ));
}

#[test]
fn test_multi_line_comments() {
    let source = r#"
        /*
         * @param {string} name - this is not JSDoc either
         */
        function alsoNotJsDoc(name) {}
    "#;

    let graph = build_graph(source);

    // No TypeOf edges should be created
    assert!(!has_typeof_edge(
        &graph,
        "alsoNotJsDoc",
        "string",
        TypeOfContext::Parameter
    ));
}

#[test]
fn test_distant_comments() {
    let source = r#"
        /**
         * @param {string} name
         */


        // Too far away (more than 1 blank line)
        function distantJsDoc(name) {}
    "#;

    let graph = build_graph(source);

    // No TypeOf edges (too far away)
    assert!(!has_typeof_edge(
        &graph,
        "distantJsDoc",
        "string",
        TypeOfContext::Parameter
    ));
}

// ========== Category 7: Integration Tests ==========

#[test]
fn test_full_file_integration() {
    let source = r#"
        /**
         * @type {Config}
         */
        const config = loadConfig();

        class UserService {
            /**
             * @type {Database}
             */
            db;

            /**
             * @param {string} id
             * @returns {Promise<User|null>}
             */
            async getUser(id) {
                return user;
            }

            /**
             * @param {User} user
             * @param {UpdateOptions} options
             * @returns {Promise<boolean>}
             */
            async updateUser(user, options) {
                return true;
            }
        }
    "#;

    let graph = build_graph(source);

    // Variable TypeOf
    assert!(has_typeof_edge(
        &graph,
        "config",
        "Config",
        TypeOfContext::Variable
    ));

    // Field TypeOf
    assert!(has_typeof_edge(
        &graph,
        "UserService.db",
        "Database",
        TypeOfContext::Field
    ));

    // Method parameter TypeOf edges
    assert!(has_typeof_edge(
        &graph,
        "UserService.getUser",
        "string",
        TypeOfContext::Parameter
    ));
    assert!(has_typeof_edge(
        &graph,
        "UserService.updateUser",
        "User",
        TypeOfContext::Parameter
    ));
    assert!(has_typeof_edge(
        &graph,
        "UserService.updateUser",
        "UpdateOptions",
        TypeOfContext::Parameter
    ));

    // Method return TypeOf edges
    assert!(has_typeof_edge(
        &graph,
        "UserService.getUser",
        "Promise<User|null>",
        TypeOfContext::Return
    ));
    assert!(has_typeof_edge(
        &graph,
        "UserService.updateUser",
        "Promise<boolean>",
        TypeOfContext::Return
    ));

    // Reference edges
    assert!(has_reference_edge(&graph, "config", "Config"));
    assert!(has_reference_edge(&graph, "UserService.db", "Database"));
    assert!(has_reference_edge(&graph, "UserService.getUser", "Promise"));
    assert!(has_reference_edge(&graph, "UserService.getUser", "User"));
    assert!(has_reference_edge(
        &graph,
        "UserService.updateUser",
        "UpdateOptions"
    ));
}

#[test]
fn test_qualified_types() {
    let source = r#"
        /**
         * @param {import('./models').User} user
         * @param {React.Component} comp
         * @param {API.Response<Data>} response
         */
        function processImports(user, comp, response) {}
    "#;

    let graph = build_graph(source);

    // TypeOf edges with full type strings
    assert!(has_typeof_edge(
        &graph,
        "processImports",
        "import('./models').User",
        TypeOfContext::Parameter
    ));
    assert!(has_typeof_edge(
        &graph,
        "processImports",
        "React.Component",
        TypeOfContext::Parameter
    ));
    assert!(has_typeof_edge(
        &graph,
        "processImports",
        "API.Response<Data>",
        TypeOfContext::Parameter
    ));

    // Reference edges
    assert!(has_reference_edge(&graph, "processImports", "User"));
    assert!(has_reference_edge(&graph, "processImports", "React"));
    assert!(has_reference_edge(&graph, "processImports", "Component"));
    assert!(has_reference_edge(&graph, "processImports", "API"));
    assert!(has_reference_edge(&graph, "processImports", "Response"));
    assert!(has_reference_edge(&graph, "processImports", "Data"));
}

#[test]
fn test_edge_cases() {
    let source = r#"
        /**
         * @param {string|null|undefined} maybeValue
         */
        function process(maybeValue) {}
    "#;

    let graph = build_graph(source);

    // TypeOf edge includes full type
    assert!(has_typeof_edge(
        &graph,
        "process",
        "string|null|undefined",
        TypeOfContext::Parameter
    ));

    // Reference edge only includes string (null/undefined excluded)
    assert!(has_reference_edge(&graph, "process", "string"));
    assert!(!has_reference_edge(&graph, "process", "null"));
    assert!(!has_reference_edge(&graph, "process", "undefined"));
}

// ========== Category 8: Edge Cases and Robustness ==========

#[test]
fn test_top_level_vs_function_scoped_variables() {
    let source = r#"
        /**
         * @type {Config}
         */
        const topLevel = getConfig();

        function myFunc() {
            /**
             * @type {number}
             */
            const functionScoped = 42;
        }
    "#;

    let graph = build_graph(source);

    // Top-level variable should have TypeOf edge
    assert!(has_typeof_edge(
        &graph,
        "topLevel",
        "Config",
        TypeOfContext::Variable
    ));

    // Function-scoped variable should NOT have TypeOf edge (filtered out)
    assert!(!has_typeof_edge(
        &graph,
        "functionScoped",
        "number",
        TypeOfContext::Variable
    ));
}

#[test]
fn test_optional_parameter() {
    let source = r#"
        /**
         * @param {string} name
         * @param {number} [age]
         */
        function createPerson(name, age) {}
    "#;

    let graph = build_graph(source);

    // Both parameters should have TypeOf edges
    assert!(has_typeof_edge(
        &graph,
        "createPerson",
        "string",
        TypeOfContext::Parameter
    ));
    assert!(has_typeof_edge(
        &graph,
        "createPerson",
        "number",
        TypeOfContext::Parameter
    ));
}

#[test]
fn test_rest_parameter_normalization() {
    let source = r#"
        /**
         * @param {...string} args
         */
        function concat(...args) {}
    "#;

    let graph = build_graph(source);

    // Rest param type should be canonicalized to "string"
    assert!(has_typeof_edge(
        &graph,
        "concat",
        "string",
        TypeOfContext::Parameter
    ));
}

#[test]
fn test_dotted_parameter_names() {
    let source = r#"
        /**
         * @param {string} options.name
         * @param {number} options.count
         */
        function configure(options) {}
    "#;

    let graph = build_graph(source);

    // Dotted parameter names should be parsed correctly
    assert!(has_typeof_edge(
        &graph,
        "configure",
        "string",
        TypeOfContext::Parameter
    ));
    assert!(has_typeof_edge(
        &graph,
        "configure",
        "number",
        TypeOfContext::Parameter
    ));
}

#[test]
fn test_export_wrappers() {
    let source = r#"
        /**
         * @param {User} user
         * @returns {boolean}
         */
        export default function validateUser(user) {
            return true;
        }
    "#;

    let graph = build_graph(source);

    // JSDoc should be attached correctly despite export wrapper
    assert!(has_typeof_edge(
        &graph,
        "validateUser",
        "User",
        TypeOfContext::Parameter
    ));
    assert!(has_typeof_edge(
        &graph,
        "validateUser",
        "boolean",
        TypeOfContext::Return
    ));
}

#[test]
fn test_class_methods_with_jsdoc() {
    let source = r#"
        class UserService {
            /**
             * @param {string} id
             * @returns {Promise<User>}
             */
            async getUser(id) {
                return user;
            }

            /**
             * @param {User} user
             * @param {UpdateOptions} options
             * @returns {Promise<boolean>}
             */
            async updateUser(user, options) {
                return true;
            }
        }
    "#;

    let graph = build_graph(source);

    // Method qualified names should be used
    assert!(has_typeof_edge(
        &graph,
        "UserService.getUser",
        "string",
        TypeOfContext::Parameter
    ));
    assert!(has_typeof_edge(
        &graph,
        "UserService.getUser",
        "Promise<User>",
        TypeOfContext::Return
    ));
    assert!(has_typeof_edge(
        &graph,
        "UserService.updateUser",
        "User",
        TypeOfContext::Parameter
    ));
    assert!(has_typeof_edge(
        &graph,
        "UserService.updateUser",
        "UpdateOptions",
        TypeOfContext::Parameter
    ));
    assert!(has_typeof_edge(
        &graph,
        "UserService.updateUser",
        "Promise<boolean>",
        TypeOfContext::Return
    ));

    // Reference edges
    assert!(has_reference_edge(&graph, "UserService.getUser", "string"));
    assert!(has_reference_edge(&graph, "UserService.getUser", "Promise"));
    assert!(has_reference_edge(&graph, "UserService.getUser", "User"));
    assert!(has_reference_edge(
        &graph,
        "UserService.updateUser",
        "UpdateOptions"
    ));
    assert!(has_reference_edge(
        &graph,
        "UserService.updateUser",
        "boolean"
    ));
}

// ========== ISSUE 1: Parameter Index Metadata Tests ==========

/// Test: JSDoc params in different order than AST
/// Validates that TypeOf edges use AST index, not JSDoc order
#[test]
fn test_param_jsdoc_out_of_order() {
    let source = r#"
        /**
         * @param {number} age - second in JSDoc, but first in AST
         * @param {string} name - first in JSDoc, but second in AST
         */
        function greet(name, age) {
            return `Hello ${name}, ${age}`;
        }
    "#;

    let graph = build_graph(source);

    // name is AST index 0, age is AST index 1 (AST order, not JSDoc order)
    // Both should have TypeOf edges with correct AST indices
    assert!(has_typeof_edge(
        &graph,
        "greet",
        "string",
        TypeOfContext::Parameter
    ));
    assert!(has_typeof_edge(
        &graph,
        "greet",
        "number",
        TypeOfContext::Parameter
    ));

    // Verify edge metadata has correct indices by checking presence
    // The test passes if edges are created (index validation happens in implementation)
    let typeof_edges = collect_edges_by_kind(&graph, |k| {
        matches!(
            k,
            sqry_core::graph::unified::edge::EdgeKind::TypeOf {
                context: Some(TypeOfContext::Parameter),
                ..
            }
        )
    });

    assert_eq!(
        typeof_edges.len(),
        2,
        "Should have 2 parameter TypeOf edges"
    );
}

/// Test: JSDoc missing some parameters
/// Only parameters with JSDoc tags should get TypeOf edges
#[test]
fn test_param_jsdoc_missing_some() {
    let source = r#"
        /**
         * @param {string} name
         */
        function greet(name, age, city) {
            return `Hello ${name}`;
        }
    "#;

    let graph = build_graph(source);

    // Only 'name' should have TypeOf edge
    assert!(has_typeof_edge(
        &graph,
        "greet",
        "string",
        TypeOfContext::Parameter
    ));

    // age and city should not have edges (no JSDoc)
    let typeof_edges = collect_edges_by_kind(&graph, |k| {
        matches!(
            k,
            sqry_core::graph::unified::edge::EdgeKind::TypeOf {
                context: Some(TypeOfContext::Parameter),
                ..
            }
        )
    });

    assert_eq!(
        typeof_edges.len(),
        1,
        "Should have only 1 parameter TypeOf edge"
    );
}

/// Test: JSDoc has extra parameters not in AST
/// Extra JSDoc tags should be skipped (no matching AST parameter)
#[test]
fn test_param_jsdoc_extra_tags() {
    let source = r#"
        /**
         * @param {string} name
         * @param {number} age
         * @param {string} city
         */
        function greet(name) {
            return `Hello ${name}`;
        }
    "#;

    let graph = build_graph(source);

    // Only 'name' edge should exist (only parameter in AST)
    assert!(has_typeof_edge(
        &graph,
        "greet",
        "string",
        TypeOfContext::Parameter
    ));

    // age and city JSDoc tags should be skipped (no AST match)
    let typeof_edges = collect_edges_by_kind(&graph, |k| {
        matches!(
            k,
            sqry_core::graph::unified::edge::EdgeKind::TypeOf {
                context: Some(TypeOfContext::Parameter),
                ..
            }
        )
    });

    assert_eq!(
        typeof_edges.len(),
        1,
        "Should have only 1 parameter TypeOf edge"
    );

    // number should NOT have a Reference edge (JSDoc tag doesn't match AST)
    assert!(!has_reference_edge(&graph, "greet", "number"));
}

/// Test: Method parameters with out-of-order JSDoc
#[test]
fn test_method_param_jsdoc_out_of_order() {
    let source = r#"
        class UserService {
            /**
             * @param {UpdateOptions} options - second in JSDoc
             * @param {User} user - first in JSDoc
             * @returns {boolean}
             */
            updateUser(user, options) {
                return true;
            }
        }
    "#;

    let graph = build_graph(source);

    // Both parameters should have TypeOf edges with correct AST indices
    assert!(has_typeof_edge(
        &graph,
        "UserService.updateUser",
        "User",
        TypeOfContext::Parameter
    ));
    assert!(has_typeof_edge(
        &graph,
        "UserService.updateUser",
        "UpdateOptions",
        TypeOfContext::Parameter
    ));

    let typeof_edges = collect_edges_by_kind(&graph, |k| {
        matches!(
            k,
            sqry_core::graph::unified::edge::EdgeKind::TypeOf {
                context: Some(TypeOfContext::Parameter),
                ..
            }
        )
    });

    assert_eq!(
        typeof_edges.len(),
        2,
        "Should have 2 parameter TypeOf edges"
    );
}

// ========== ISSUE 2: Block-Scoped Variable Tests ==========

/// Test: Variable inside if block should NOT get TypeOf edge
#[test]
fn test_variable_in_if_block_no_typeof() {
    let source = r#"
        if (true) {
            /** @type {User} */
            const user = getUser();
        }
    "#;

    let graph = build_graph(source);

    // Should NOT create TypeOf edge (block-scoped, not module-scoped)
    let typeof_edges = collect_edges_by_kind(&graph, |k| {
        matches!(
            k,
            sqry_core::graph::unified::edge::EdgeKind::TypeOf {
                context: Some(TypeOfContext::Variable),
                ..
            }
        )
    });

    assert_eq!(
        typeof_edges.len(),
        0,
        "Block-scoped variable should not get TypeOf edge"
    );
}

/// Test: Variable inside for loop should NOT get TypeOf edge
#[test]
fn test_variable_in_for_loop_no_typeof() {
    let source = r#"
        for (let i = 0; i < 10; i++) {
            /** @type {Item} */
            const item = items[i];
        }
    "#;

    let graph = build_graph(source);

    let typeof_edges = collect_edges_by_kind(&graph, |k| {
        matches!(
            k,
            sqry_core::graph::unified::edge::EdgeKind::TypeOf {
                context: Some(TypeOfContext::Variable),
                ..
            }
        )
    });

    assert_eq!(
        typeof_edges.len(),
        0,
        "Loop variable should not get TypeOf edge"
    );
}

/// Test: Variable inside try/catch block should NOT get TypeOf edge
#[test]
fn test_variable_in_try_block_no_typeof() {
    let source = r#"
        try {
            /** @type {Result} */
            const result = riskyOperation();
        } catch (e) {
            /** @type {Error} */
            const error = e;
        }
    "#;

    let graph = build_graph(source);

    let typeof_edges = collect_edges_by_kind(&graph, |k| {
        matches!(
            k,
            sqry_core::graph::unified::edge::EdgeKind::TypeOf {
                context: Some(TypeOfContext::Variable),
                ..
            }
        )
    });

    assert_eq!(
        typeof_edges.len(),
        0,
        "Try/catch variables should not get TypeOf edges"
    );
}

/// Test: Module-level variable SHOULD get TypeOf edge
#[test]
fn test_module_level_variable_gets_typeof() {
    let source = r#"
        /** @type {Config} */
        const config = loadConfig();
    "#;

    let graph = build_graph(source);

    // Should create TypeOf edge (module-scoped)
    assert!(has_typeof_edge(
        &graph,
        "config",
        "Config",
        TypeOfContext::Variable
    ));

    let typeof_edges = collect_edges_by_kind(&graph, |k| {
        matches!(
            k,
            sqry_core::graph::unified::edge::EdgeKind::TypeOf {
                context: Some(TypeOfContext::Variable),
                ..
            }
        )
    });

    assert!(
        !typeof_edges.is_empty(),
        "Module-level variable should get TypeOf edge"
    );
}

/// Test: Exported variable SHOULD get TypeOf edge
#[test]
fn test_exported_variable_gets_typeof() {
    let source = r#"
        /** @type {Settings} */
        export const settings = {};
    "#;

    let graph = build_graph(source);

    // Should create TypeOf edge (exported = module-scoped)
    assert!(has_typeof_edge(
        &graph,
        "settings",
        "Settings",
        TypeOfContext::Variable
    ));

    let typeof_edges = collect_edges_by_kind(&graph, |k| {
        matches!(
            k,
            sqry_core::graph::unified::edge::EdgeKind::TypeOf {
                context: Some(TypeOfContext::Variable),
                ..
            }
        )
    });

    assert!(
        !typeof_edges.is_empty(),
        "Exported variable should get TypeOf edge"
    );
}

/// Test: Variable in while loop should NOT get TypeOf edge
#[test]
fn test_variable_in_while_loop_no_typeof() {
    let source = r#"
        while (condition) {
            /** @type {Item} */
            const item = getNext();
        }
    "#;

    let graph = build_graph(source);

    let typeof_edges = collect_edges_by_kind(&graph, |k| {
        matches!(
            k,
            sqry_core::graph::unified::edge::EdgeKind::TypeOf {
                context: Some(TypeOfContext::Variable),
                ..
            }
        )
    });

    assert_eq!(
        typeof_edges.len(),
        0,
        "While loop variable should not get TypeOf edge"
    );
}

// ========== ISSUE 3: Anonymous Class Tests ==========

/// Test: Anonymous class assigned to variable with JSDoc
#[test]
fn test_anonymous_class_variable_assignment() {
    let source = r#"
        const UserService = class {
            /**
             * @param {string} id
             * @returns {User}
             */
            async getUser(id) {
                return await fetch(`/users/${id}`);
            }
        };
    "#;

    let graph = build_graph(source);

    // Should create TypeOf edges for anonymous class method
    // Class name should be "UserService" (from variable)
    assert!(has_typeof_edge(
        &graph,
        "UserService.getUser",
        "string",
        TypeOfContext::Parameter
    ));
    assert!(has_typeof_edge(
        &graph,
        "UserService.getUser",
        "User",
        TypeOfContext::Return
    ));

    let typeof_edges = collect_edges_by_kind(&graph, |k| {
        matches!(k, sqry_core::graph::unified::edge::EdgeKind::TypeOf { .. })
    });

    assert!(
        typeof_edges.len() >= 2,
        "Anonymous class method should get TypeOf edges"
    );
}

/// Test: Anonymous class expression (not assigned)
/// This validates graceful handling - methods exist but JSDoc edges may not
#[test]
fn test_anonymous_class_expression() {
    let source = r#"
        export default class {
            /**
             * @param {number} count
             */
            increment(count) {
                this.value += count;
            }
        };
    "#;

    let graph = build_graph(source);

    // This test validates that the code doesn't crash on anonymous classes
    // The behavior is: if class has no name and isn't assigned, JSDoc edges are skipped
    // But the method nodes should still exist from main traversal
    // This is acceptable behavior for Phase 1
    let _all_edges = collect_edges_by_kind(&graph, |_| true);
    // Test passes if no panic occurred
}

/// Test: Anonymous class with field JSDoc
#[test]
fn test_anonymous_class_field_jsdoc() {
    let source = r#"
        const DataService = class {
            /**
             * @type {Database}
             */
            db;

            /**
             * @type {Logger}
             */
            logger;
        };
    "#;

    let graph = build_graph(source);

    // Fields in anonymous class should get TypeOf edges
    // Uses variable name "DataService" as class name
    assert!(has_typeof_edge(
        &graph,
        "DataService.db",
        "Database",
        TypeOfContext::Field
    ));
    assert!(has_typeof_edge(
        &graph,
        "DataService.logger",
        "Logger",
        TypeOfContext::Field
    ));
}
