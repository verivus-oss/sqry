use sqry_core::ast::Scope;
use sqry_core::graph::node::Language;
use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
use sqry_core::graph::unified::edge::{
    DbQueryType, EdgeKind, ExportKind, FfiConvention, LifetimeConstraintKind, MacroExpansionKind,
    MqProtocol, TableWriteOp,
};
use sqry_core::graph::unified::{GraphSnapshot, NodeId, StringId};
use sqry_core::graph::{GraphBuilder, GraphBuilderError, GraphResult};
use sqry_core::plugin::error::{ParseError, ScopeError};
use sqry_core::plugin::{LanguageMetadata, LanguagePlugin};
use sqry_plugin_registry::create_plugin_manager;
use std::fs;
use std::path::Path;
use tempfile::TempDir;
use tree_sitter::{Parser, Tree};

fn get_fixture_path() -> &'static Path {
    let p = Path::new("test-fixtures/cross-language-example");
    if p.exists() {
        return p;
    }
    let p = Path::new("../test-fixtures/cross-language-example");
    if p.exists() {
        return p;
    }
    panic!("Fixture not found at ./test-fixtures or ../test-fixtures");
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct EdgeDigest {
    source: String,
    target: String,
    kind: String,
    file_path: String,
    span_start: u32,
    span_end: u32,
    metadata: String,
}

fn normalized_edge_digests(snapshot: &GraphSnapshot, root: &Path) -> Vec<EdgeDigest> {
    let mut digests = Vec::new();

    for (node_id, _entry) in snapshot.nodes().iter() {
        for edge in snapshot.edges().edges_from(node_id) {
            let source_key = node_sort_key(snapshot, edge.source);
            let target_key = node_sort_key(snapshot, edge.target);
            let file_path = snapshot
                .files()
                .resolve(edge.file)
                .map(|path| normalize_path_for_snapshot(root, path.as_ref()))
                .unwrap_or_else(|| "<missing>".to_string());
            let metadata = edge_metadata_summary(snapshot, &edge.kind);

            digests.push(EdgeDigest {
                source: source_key,
                target: target_key,
                kind: edge.kind.tag().to_string(),
                file_path,
                span_start: 0,
                span_end: 0,
                metadata,
            });
        }
    }

    digests.sort();
    digests
}

fn node_sort_key(snapshot: &GraphSnapshot, node_id: NodeId) -> String {
    let entry = snapshot.nodes().get(node_id);
    let Some(entry) = entry else {
        return "<missing>".to_string();
    };

    if let Some(qualified_name) = entry
        .qualified_name
        .and_then(|id| snapshot.strings().resolve(id))
    {
        return qualified_name.as_ref().to_string();
    }

    snapshot
        .strings()
        .resolve(entry.name)
        .map(|name| name.as_ref().to_string())
        .unwrap_or_else(|| "<missing>".to_string())
}

fn resolve_string(snapshot: &GraphSnapshot, id: StringId) -> String {
    snapshot
        .strings()
        .resolve(id)
        .map(|value| value.as_ref().to_string())
        .unwrap_or_else(|| "<missing>".to_string())
}

fn resolve_optional_string(snapshot: &GraphSnapshot, id: Option<StringId>) -> String {
    id.map(|value| resolve_string(snapshot, value))
        .unwrap_or_else(|| "<none>".to_string())
}

fn normalize_path_for_snapshot(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    relative.to_string_lossy().replace('\\', "/")
}

fn edge_metadata_summary(snapshot: &GraphSnapshot, kind: &EdgeKind) -> String {
    match kind {
        EdgeKind::Defines => "defines".to_string(),
        EdgeKind::Contains => "contains".to_string(),
        EdgeKind::Calls {
            argument_count,
            is_async,
        } => format!("calls|argument_count={argument_count}|is_async={is_async}"),
        EdgeKind::References => "references".to_string(),
        EdgeKind::Imports { alias, is_wildcard } => format!(
            "imports|alias={}|is_wildcard={is_wildcard}",
            resolve_optional_string(snapshot, *alias)
        ),
        EdgeKind::Exports { kind, alias } => format!(
            "exports|kind={}|alias={}",
            export_kind_name(*kind),
            resolve_optional_string(snapshot, *alias)
        ),
        EdgeKind::TypeOf { .. } => "type_of".to_string(),
        EdgeKind::Inherits => "inherits".to_string(),
        EdgeKind::Implements => "implements".to_string(),
        EdgeKind::FfiCall { convention } => {
            format!("ffi_call|convention={}", ffi_convention_name(*convention))
        }
        EdgeKind::HttpRequest { method, url } => format!(
            "http_request|method={}|url={}",
            method.as_str(),
            resolve_optional_string(snapshot, *url)
        ),
        EdgeKind::GrpcCall { service, method } => format!(
            "grpc_call|service={}|method={}",
            resolve_string(snapshot, *service),
            resolve_string(snapshot, *method)
        ),
        EdgeKind::WebAssemblyCall => "web_assembly_call".to_string(),
        EdgeKind::DbQuery { query_type, table } => format!(
            "db_query|query_type={}|table={}",
            db_query_type_name(*query_type),
            resolve_optional_string(snapshot, *table)
        ),
        EdgeKind::TableRead { table_name, schema } => format!(
            "table_read|table_name={}|schema={}",
            resolve_string(snapshot, *table_name),
            resolve_optional_string(snapshot, *schema)
        ),
        EdgeKind::TableWrite {
            table_name,
            schema,
            operation,
        } => format!(
            "table_write|table_name={}|schema={}|operation={}",
            resolve_string(snapshot, *table_name),
            resolve_optional_string(snapshot, *schema),
            table_write_op_name(*operation)
        ),
        EdgeKind::TriggeredBy {
            trigger_name,
            schema,
        } => format!(
            "triggered_by|trigger_name={}|schema={}",
            resolve_string(snapshot, *trigger_name),
            resolve_optional_string(snapshot, *schema)
        ),
        EdgeKind::MessageQueue { protocol, topic } => format!(
            "message_queue|protocol={}|topic={}",
            mq_protocol_name(snapshot, protocol),
            resolve_optional_string(snapshot, *topic)
        ),
        EdgeKind::WebSocket { event } => format!(
            "web_socket|event={}",
            resolve_optional_string(snapshot, *event)
        ),
        EdgeKind::GraphQLOperation { operation } => format!(
            "graphql_operation|operation={}",
            resolve_string(snapshot, *operation)
        ),
        EdgeKind::ProcessExec { command } => format!(
            "process_exec|command={}",
            resolve_string(snapshot, *command)
        ),
        EdgeKind::FileIpc { path_pattern } => format!(
            "file_ipc|path_pattern={}",
            resolve_optional_string(snapshot, *path_pattern)
        ),
        EdgeKind::ProtocolCall { protocol, metadata } => format!(
            "protocol_call|protocol={}|metadata={}",
            resolve_string(snapshot, *protocol),
            resolve_optional_string(snapshot, *metadata)
        ),
        EdgeKind::LifetimeConstraint { constraint_kind } => format!(
            "lifetime_constraint|kind={}",
            lifetime_constraint_kind_name(*constraint_kind)
        ),
        EdgeKind::TraitMethodBinding {
            trait_name,
            impl_type,
            is_ambiguous,
        } => format!(
            "trait_method_binding|trait={}|impl_type={}|is_ambiguous={}",
            resolve_string(snapshot, *trait_name),
            resolve_string(snapshot, *impl_type),
            is_ambiguous
        ),
        EdgeKind::MacroExpansion {
            expansion_kind,
            is_verified,
        } => format!(
            "macro_expansion|kind={}|is_verified={}",
            macro_expansion_kind_name(*expansion_kind),
            is_verified
        ),
    }
}

fn export_kind_name(kind: ExportKind) -> &'static str {
    match kind {
        ExportKind::Direct => "direct",
        ExportKind::Reexport => "reexport",
        ExportKind::Default => "default",
        ExportKind::Namespace => "namespace",
    }
}

fn ffi_convention_name(convention: FfiConvention) -> &'static str {
    match convention {
        FfiConvention::C => "c",
        FfiConvention::Cdecl => "cdecl",
        FfiConvention::Stdcall => "stdcall",
        FfiConvention::Fastcall => "fastcall",
        FfiConvention::System => "system",
    }
}

fn db_query_type_name(query_type: DbQueryType) -> &'static str {
    match query_type {
        DbQueryType::Select => "select",
        DbQueryType::Insert => "insert",
        DbQueryType::Update => "update",
        DbQueryType::Delete => "delete",
        DbQueryType::Execute => "execute",
    }
}

fn table_write_op_name(operation: TableWriteOp) -> &'static str {
    match operation {
        TableWriteOp::Insert => "insert",
        TableWriteOp::Update => "update",
        TableWriteOp::Delete => "delete",
    }
}

fn mq_protocol_name(snapshot: &GraphSnapshot, protocol: &MqProtocol) -> String {
    match protocol {
        MqProtocol::Kafka => "kafka".to_string(),
        MqProtocol::Sqs => "sqs".to_string(),
        MqProtocol::RabbitMq => "rabbitmq".to_string(),
        MqProtocol::Nats => "nats".to_string(),
        MqProtocol::Redis => "redis".to_string(),
        MqProtocol::Other(id) => resolve_string(snapshot, *id),
    }
}

fn lifetime_constraint_kind_name(kind: LifetimeConstraintKind) -> &'static str {
    match kind {
        LifetimeConstraintKind::Outlives => "outlives",
        LifetimeConstraintKind::TypeBound => "type_bound",
        LifetimeConstraintKind::Reference => "reference",
        LifetimeConstraintKind::Static => "static",
        LifetimeConstraintKind::HigherRanked => "higher_ranked",
        LifetimeConstraintKind::TraitObject => "trait_object",
        LifetimeConstraintKind::ImplTrait => "impl_trait",
        LifetimeConstraintKind::Elided => "elided",
    }
}

fn macro_expansion_kind_name(kind: MacroExpansionKind) -> &'static str {
    match kind {
        MacroExpansionKind::Derive => "derive",
        MacroExpansionKind::Attribute => "attribute",
        MacroExpansionKind::Declarative => "declarative",
        MacroExpansionKind::Function => "function",
        MacroExpansionKind::CfgGate => "cfg_gate",
    }
}

#[derive(Default)]
struct FailingGraphBuilder;

impl GraphBuilder for FailingGraphBuilder {
    fn build_graph(
        &self,
        _tree: &Tree,
        _content: &[u8],
        _file: &Path,
        _staging: &mut sqry_core::graph::unified::build::StagingGraph,
    ) -> GraphResult<()> {
        Err(GraphBuilderError::CrossLanguageError {
            reason: "intentional failure".to_string(),
        })
    }

    fn language(&self) -> Language {
        Language::Rust
    }
}

#[derive(Default)]
struct FailingPlugin {
    builder: FailingGraphBuilder,
}

impl LanguagePlugin for FailingPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: "test-failing",
            name: "TestFailing",
            version: "0.1.0",
            author: "sqry",
            description: "Test-only failing plugin",
            tree_sitter_version: "0.23",
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["bad"]
    }

    fn language(&self) -> tree_sitter::Language {
        tree_sitter_rust::LANGUAGE.into()
    }

    fn parse_ast(&self, content: &[u8]) -> Result<Tree, ParseError> {
        let mut parser = Parser::new();
        parser.set_language(&self.language()).map_err(|e| {
            ParseError::LanguageSetFailed(format!("Failed to set test language: {e}"))
        })?;
        parser
            .parse(content, None)
            .ok_or(ParseError::TreeSitterFailed)
    }

    fn extract_scopes(
        &self,
        _tree: &Tree,
        _content: &[u8],
        _file_path: &Path,
    ) -> Result<Vec<Scope>, ScopeError> {
        Ok(Vec::new())
    }

    fn graph_builder(&self) -> Option<&dyn GraphBuilder> {
        Some(&self.builder)
    }
}

#[test]
fn test_entrypoint_builds_non_empty_graph() {
    let root = get_fixture_path();
    let plugins = create_plugin_manager();
    let config = BuildConfig::default();

    let graph = build_unified_graph(root, &plugins, &config).expect("Build failed");

    assert!(
        graph.node_count() > 0,
        "Graph should not be empty, got {} nodes",
        graph.node_count()
    );
    assert!(
        graph.edge_count() > 0,
        "Graph should have edges, got {} edges",
        graph.edge_count()
    );
}

#[test]
fn test_entrypoint_fails_with_empty_registry() {
    let root = get_fixture_path();
    let plugins = sqry_core::plugin::PluginManager::new();
    let config = BuildConfig::default();

    let result = build_unified_graph(root, &plugins, &config);
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().to_string(),
        "No graph builders registered – cannot build code graph"
    );
}

#[test]
fn test_entrypoint_skips_failed_file() {
    let temp = TempDir::new().expect("temp dir");
    fs::write(temp.path().join("ok.rs"), "fn ok() {}\n").expect("write ok file");
    fs::write(temp.path().join("bad.bad"), "fn bad() {}\n").expect("write bad file");

    let mut plugins = create_plugin_manager();
    plugins.register_builtin(Box::new(FailingPlugin::default()));
    let config = BuildConfig::default();

    let graph =
        build_unified_graph(temp.path(), &plugins, &config).expect("Build should skip failed file");

    assert_eq!(
        graph.files().len(),
        1,
        "Expected only the ok file to register"
    );
}

#[test]
fn test_entrypoint_determinism() {
    let root = get_fixture_path();
    let plugins = create_plugin_manager();
    let config = BuildConfig::default();

    let graph1 = build_unified_graph(root, &plugins, &config).expect("Build 1 failed");
    let graph2 = build_unified_graph(root, &plugins, &config).expect("Build 2 failed");

    let snapshot1 = graph1.snapshot();
    let snapshot2 = graph2.snapshot();

    let digest1 = normalized_edge_digests(&snapshot1, root);
    let digest2 = normalized_edge_digests(&snapshot2, root);

    assert_eq!(digest1, digest2, "Determinism digest mismatch");
}

#[test]
fn test_entrypoint_parity_with_cli_loader() {
    use sqry_cli::commands::graph::loader::{GraphLoadConfig, load_unified_graph};

    let root = get_fixture_path();
    let plugins = create_plugin_manager();
    let build_config = BuildConfig::default();

    let core_graph = build_unified_graph(root, &plugins, &build_config).expect("Core build failed");

    let load_config = GraphLoadConfig {
        force_build: true,
        ..Default::default()
    };
    let cli_graph = load_unified_graph(root, &load_config).expect("CLI load failed");

    let core_snapshot = core_graph.snapshot();
    let cli_snapshot = cli_graph.snapshot();

    let core_digest = normalized_edge_digests(&core_snapshot, root);
    let cli_digest = normalized_edge_digests(&cli_snapshot, root);

    assert_eq!(
        core_graph.node_count(),
        cli_graph.node_count(),
        "Node count mismatch"
    );
    assert_eq!(
        core_graph.edge_count(),
        cli_graph.edge_count(),
        "Edge count mismatch"
    );
    assert_eq!(core_digest, cli_digest, "Edge digest mismatch");
}
