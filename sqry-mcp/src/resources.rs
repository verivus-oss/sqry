//! MCP Resources — on-demand documentation for sqry tools.
//!
//! Layer 2 of the two-layer documentation strategy:
//! - **Layer 1** (`instructions`): ~200 token quick-reference, always in context
//! - **Layer 2** (resources): Detailed docs, fetched on demand via `resources/read`
//!
//! Documentation follows token-optimization principles (see `TOKEN_OPTIMIZATION_GUIDE.md`):
//! - Terse descriptions (verb + object, ≤12 words)
//! - Inline constraints (`param:type!constraint`)
//! - Markdown format (16% savings vs JSON, +10% comprehension)
//! - Self-contained sections (256-512 token chunks)

use rmcp::model::{RawResource, Resource, ResourceContents};

/// All available documentation resource URIs.
pub const RESOURCE_TOOL_GUIDE: &str = "sqry://docs/tool-guide";
pub const RESOURCE_QUERY_SYNTAX: &str = "sqry://docs/query-syntax";
pub const RESOURCE_PATTERNS: &str = "sqry://docs/patterns";
pub const RESOURCE_ARCHITECTURE: &str = "sqry://docs/architecture";
pub const RESOURCE_MANIFEST: &str = "sqry://meta/manifest";
pub const RESOURCE_CAPABILITY_MAP: &str = "sqry://docs/capability-map";

/// Compiled capability counts — validated by tests against actual registries.
/// Full relation support languages (28) + symbol extraction languages (9) = 37 total.
pub const LANGUAGE_COUNT_TOTAL: u32 = 37;
pub const LANGUAGE_COUNT_FULL_RELATION: u32 = 28;
pub const LANGUAGE_COUNT_SYMBOL_EXTRACTION: u32 = 9;
pub const SNAPSHOT_FORMAT_VERSION: u32 = 7;
pub const DEFAULT_QUERY_TIMEOUT_MS: u64 = 60_000;
pub const DEFAULT_INDEX_TIMEOUT_MS: u64 = 600_000;
pub const DEFAULT_REDACTION_PRESET: &str = "minimal";

/// Build the list of available documentation resources.
pub fn list_resources() -> Vec<Resource> {
    vec![
        make_resource(
            RESOURCE_TOOL_GUIDE,
            "Tool Selection Guide",
            "Complete tool reference grouped by task with parameters and examples",
        ),
        make_resource(
            RESOURCE_QUERY_SYNTAX,
            "Query Syntax Reference",
            "Semantic search query language: fields, operators, combinators, examples",
        ),
        make_resource(
            RESOURCE_PATTERNS,
            "Common Usage Patterns",
            "Workflow recipes: investigate symbol, impact analysis, code review, onboarding",
        ),
        make_resource(
            RESOURCE_ARCHITECTURE,
            "Graph Architecture",
            "Arena+CSR storage, node/edge kinds, build pipeline, concurrency model",
        ),
        make_json_resource(
            RESOURCE_MANIFEST,
            "sqry Manifest",
            "Machine-readable JSON: version, tool count, language count, snapshot format, defaults",
        ),
        make_resource(
            RESOURCE_CAPABILITY_MAP,
            "Capability Map",
            "Task-oriented tool routing: what do you want to do → which tool to use",
        ),
    ]
}

/// Read a documentation resource by URI. Returns `None` for unknown URIs.
pub fn read_resource(uri: &str) -> Option<ResourceContents> {
    // Manifest is JSON; capability-map is generated markdown; rest are static markdown
    if uri == RESOURCE_MANIFEST {
        return Some(ResourceContents::TextResourceContents {
            uri: uri.to_string(),
            mime_type: Some("application/json".into()),
            text: build_manifest_json(),
            meta: None,
        });
    }

    if uri == RESOURCE_CAPABILITY_MAP {
        return Some(ResourceContents::TextResourceContents {
            uri: uri.to_string(),
            mime_type: Some("text/markdown".into()),
            text: build_capability_map(),
            meta: None,
        });
    }

    let content = match uri {
        RESOURCE_TOOL_GUIDE => {
            return Some(ResourceContents::TextResourceContents {
                uri: uri.to_string(),
                mime_type: Some("text/markdown".into()),
                text: build_tool_guide(),
                meta: None,
            });
        }
        RESOURCE_QUERY_SYNTAX => QUERY_SYNTAX_MD,
        RESOURCE_PATTERNS => PATTERNS_MD,
        RESOURCE_ARCHITECTURE => ARCHITECTURE_MD,
        _ => return None,
    };
    Some(ResourceContents::TextResourceContents {
        uri: uri.to_string(),
        mime_type: Some("text/markdown".into()),
        text: content.to_string(),
        meta: None,
    })
}

fn make_resource(uri: &str, name: &str, description: &str) -> Resource {
    let mut raw = RawResource::new(uri, name);
    raw.description = Some(description.into());
    raw.mime_type = Some("text/markdown".into());
    Resource {
        raw,
        annotations: None,
    }
}

fn make_json_resource(uri: &str, name: &str, description: &str) -> Resource {
    let mut raw = RawResource::new(uri, name);
    raw.description = Some(description.into());
    raw.mime_type = Some("application/json".into());
    Resource {
        raw,
        annotations: None,
    }
}

/// Build the manifest JSON at runtime from compile-time constants.
/// The tool count is passed from the server (derived from the tool registry),
/// so it can never drift from the actual registered tools.
///
/// Schema contract: fields are append-only — may be added, never removed or renamed.
fn build_manifest_json() -> String {
    // Tool count is set by set_tool_count() called from server initialization.
    // If not set (e.g. in unit tests), falls back to 0.
    let tools = TOOL_COUNT.load(std::sync::atomic::Ordering::Relaxed);
    format!(
        r#"{{
  "version": "{}",
  "tools": {},
  "languages": {{
    "total": {},
    "full_relation": {},
    "symbol_extraction": {}
  }},
  "snapshot_format": {},
  "defaults": {{
    "redaction_preset": "{}",
    "query_timeout_ms": {},
    "index_timeout_ms": {}
  }}
}}"#,
        env!("CARGO_PKG_VERSION"),
        tools,
        LANGUAGE_COUNT_TOTAL,
        LANGUAGE_COUNT_FULL_RELATION,
        LANGUAGE_COUNT_SYMBOL_EXTRACTION,
        SNAPSHOT_FORMAT_VERSION,
        DEFAULT_REDACTION_PRESET,
        DEFAULT_QUERY_TIMEOUT_MS,
        DEFAULT_INDEX_TIMEOUT_MS,
    )
}

/// Atomic tool count, set by the server after tool registration.
static TOOL_COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Called by the server after tool registration to set the manifest tool count.
pub fn set_tool_count(count: u32) {
    TOOL_COUNT.store(count, std::sync::atomic::Ordering::Relaxed);
}

// ============================================================================
// Capability Map — generated from tool category assignments
// ============================================================================

/// Tool category for the capability map. Each tool is assigned exactly one category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolCategory {
    Search,
    Navigation,
    Relationships,
    ImpactAnalysis,
    CodeQuality,
    VersionComparison,
    CrossLanguage,
    IndexManagement,
    Export,
    NaturalLanguage,
}

impl ToolCategory {
    fn label(self) -> &'static str {
        match self {
            Self::Search => "Find a symbol",
            Self::Navigation => "Navigate to it",
            Self::Relationships => "Trace relationships",
            Self::ImpactAnalysis => "Assess change impact",
            Self::CodeQuality => "Check code quality",
            Self::VersionComparison => "Compare versions",
            Self::CrossLanguage => "Cross-language analysis",
            Self::IndexManagement => "Inspect the index",
            Self::Export => "Export / visualize",
            Self::NaturalLanguage => "Natural language",
        }
    }

    /// Ordered list of all categories for stable output.
    fn all() -> &'static [Self] {
        &[
            Self::Search,
            Self::Navigation,
            Self::Relationships,
            Self::ImpactAnalysis,
            Self::CodeQuality,
            Self::VersionComparison,
            Self::CrossLanguage,
            Self::IndexManagement,
            Self::Export,
            Self::NaturalLanguage,
        ]
    }
}

/// Map every registered tool name to a category. This is the single source of truth
/// for the capability-map resource. If a tool is added to the server without being
/// added here, the `capability_map_contains_all_tools` test will fail.
fn tool_category(name: &str) -> ToolCategory {
    match name {
        "semantic_search"
        | "hierarchical_search"
        | "pattern_search"
        | "get_workspace_symbols"
        | "search_similar" => ToolCategory::Search,

        "get_definition" | "get_references" | "get_hover_info" | "get_document_symbols" => {
            ToolCategory::Navigation
        }

        "relation_query" | "direct_callers" | "direct_callees" | "call_hierarchy"
        | "trace_path" => ToolCategory::Relationships,

        "dependency_impact" | "show_dependencies" | "subgraph" | "explain_code" => {
            ToolCategory::ImpactAnalysis
        }

        "find_cycles" | "is_node_in_cycle" | "find_unused" | "find_duplicates"
        | "complexity_metrics" => ToolCategory::CodeQuality,

        "semantic_diff" => ToolCategory::VersionComparison,

        "cross_language_edges" => ToolCategory::CrossLanguage,

        "get_index_status"
        | "get_graph_stats"
        | "get_insights"
        | "rebuild_index"
        | "list_files"
        | "list_symbols"
        | "expand_cache_status"
        // STEP_7 — `workspace_status` belongs with index-management since it
        // returns the aggregate `WorkspaceIndexStatus` for the active
        // logical workspace.
        | "workspace_status" => ToolCategory::IndexManagement,

        "export_graph" => ToolCategory::Export,

        "sqry_ask" => ToolCategory::NaturalLanguage,

        unknown => {
            // Unknown tools must be categorized. In debug/test builds, panic to catch
            // missing entries immediately. In release, log and default to Search.
            #[cfg(any(debug_assertions, test))]
            panic!("tool_category: uncategorized tool '{unknown}' — add it to the match arms");
            #[cfg(not(any(debug_assertions, test)))]
            {
                tracing::warn!(
                    tool = unknown,
                    "uncategorized tool in capability map — defaulting to Search"
                );
                ToolCategory::Search
            }
        }
    }
}

/// Store of registered tool names, set by the server at init time.
/// Uses Mutex instead of `OnceLock` so the names can be updated if the server
/// is reconstructed (e.g. with different feature flags).
static TOOL_NAMES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Called by the server after tool registration to set tool names for the capability map.
pub fn set_tool_names(names: Vec<String>) {
    *TOOL_NAMES.lock().unwrap() = names;
}

/// Build the capability map markdown from registered tool names and their categories.
fn build_capability_map() -> String {
    let names = TOOL_NAMES.lock().unwrap().clone();

    // Group tools by category
    let mut groups: std::collections::BTreeMap<&str, Vec<&str>> = std::collections::BTreeMap::new();
    // Pre-populate all categories in order
    for cat in ToolCategory::all() {
        groups.entry(cat.label()).or_default();
    }
    for name in &names {
        let cat = tool_category(name);
        groups.entry(cat.label()).or_default().push(name);
    }

    let mut out = String::from("# What do you want to do?\n\n");
    for cat in ToolCategory::all() {
        let tools = groups.get(cat.label()).unwrap();
        if !tools.is_empty() {
            use std::fmt::Write as _;
            let _ = writeln!(out, "**{}**: {}", cat.label(), tools.join(", "));
        }
    }
    out.push_str("\nFor full parameters: sqry://docs/tool-guide\n");
    out.push_str("For query syntax: sqry://docs/query-syntax\n");
    out
}

// ============================================================================
// Layer 2 Documentation Content
//
// Token-optimized following TOKEN_OPTIMIZATION_GUIDE.md:
// - Direct verbs, no filler ("This tool is used to" -> verb)
// - Inline constraints (param:type!constraint)
// - snake_case parameters
// - Markdown tables and bullet lists
// - Self-contained sections
// ============================================================================

/// Tool guide editorial content. The tool count header is injected dynamically
/// by `build_tool_guide()` from the `TOOL_COUNT` atomic. Tool descriptions and
/// parameter schemas are editorial — they describe usage patterns, not raw schemas.
///
/// When adding a new tool: add it to the appropriate category section below,
/// add it to `tool_category()` in the capability-map section, and the tool count
/// will update automatically from the server's tool registry.
const TOOL_GUIDE_BODY: &str = "\n\
## Search

| Tool | Purpose | Key Params |
|------|---------|------------|
| `semantic_search` | Search by name/kind/visibility/language | query:str!, filters:{language?,symbol_kind?,visibility?,score_min?}?, context_lines:int? |
| `hierarchical_search` | RAG-optimized search grouped by file/container | query:str!, filters:{language?,symbol_kind?,visibility?,score_min?}?, max_files:int? |
| `pattern_search` | Find symbols by substring match | pattern:str! |
| `get_workspace_symbols` | Search symbols by name across workspace | query:str! |
| `sqry_ask` | Natural language to sqry query | query:str! |

### Filters Parameter

The `query` parameter accepts string predicates like `lang:rust`. \
The `filters` parameter accepts a **JSON object** for structured pre-filtering:

```json
{
  \"language\": [\"rust\", \"typescript\"],
  \"symbol_kind\": [\"function\", \"method\"],
  \"visibility\": \"public\",
  \"score_min\": 0.5
}
```

All fields are optional. Filters are applied before query evaluation.

## Navigation

| Tool | Purpose | Key Params |
|------|---------|------------|
| `get_definition` | Find where symbol is defined | symbol:str! |
| `get_references` | Find all references to symbol | symbol:str!, include_declaration:bool? |
| `get_hover_info` | Get signature, docs, type info | symbol:str! |
| `get_document_symbols` | List all symbols in a file | file_path:str! |

## Relationships

| Tool | Purpose | Key Params |
|------|---------|------------|
| `relation_query` | Query callers/callees/imports/exports/returns | symbol:str!, relation_type:enum! |
| `call_hierarchy` | Call tree (incoming or outgoing) | symbol:str!, direction:enum! |
| `direct_callers` | Immediate callers (depth=1) | symbol:str! |
| `direct_callees` | Immediate callees (depth=1) | symbol:str! |
| `trace_path` | Ranked call paths between two symbols | from_symbol:str!, to_symbol:str! |

## Impact Analysis

| Tool | Purpose | Key Params |
|------|---------|------------|
| `dependency_impact` | What breaks if symbol changed/removed | symbol:str!, max_depth:int? |
| `show_dependencies` | Dependency tree for file or symbol | file_path:str?, symbol_name:str? |
| `subgraph` | Focused subgraph around seed symbols | symbols:str[]! |
| `explain_code` | Symbol context with relations | file_path:str!, symbol_name:str! |
| `search_similar` | Find similar symbols (fuzzy match) | reference:{file_path,symbol_name}! |

## Code Quality

| Tool | Purpose | Key Params |
|------|---------|------------|
| `find_cycles` | Circular dependencies in calls/imports/modules | cycle_type:enum? |
| `is_node_in_cycle` | Check if symbol participates in cycle | symbol:str! |
| `find_unused` | Unreachable or unused symbols | scope:enum?, language:str[]? |
| `find_duplicates` | Duplicate functions/signatures/structs | duplicate_type:enum?, threshold:int? |
| `complexity_metrics` | Function complexity from call graph + LOC | target:str?, min_complexity:int? |

## Version Comparison

| Tool | Purpose | Key Params |
|------|---------|------------|
| `semantic_diff` | Symbol-level changes between git refs | base:{ref}!, target:{ref}! |

## Cross-Language

| Tool | Purpose | Key Params |
|------|---------|------------|
| `cross_language_edges` | List edges where caller/callee languages differ | from_lang:str?, to_lang:str? |

## Index Management

| Tool | Purpose | Key Params |
|------|---------|------------|
| `get_index_status` | Index status and metadata | - |
| `get_graph_stats` | Node, edge, file counts + language breakdown | - |
| `get_insights` | Health metrics: cycles, quality indicators | - |
| `rebuild_index` | Rebuild code graph from source | force:bool? |
| `list_files` | List indexed files | language:str? |
| `list_symbols` | List indexed symbols | kind:str?, language:str? |
| `expand_cache_status` | Macro expansion cache status (Rust) | path:str? |
| `workspace_status` | Aggregate WorkspaceIndexStatus + workspace identity | workspace_id:str? |

## Export

| Tool | Purpose | Key Params |
|------|---------|------------|
| `export_graph` | Export subgraph as JSON/DOT/D2/Mermaid | symbol_name:str?, format:enum? |
";

/// Build the tool guide with a dynamic tool count header.
fn build_tool_guide() -> String {
    let tools = TOOL_COUNT.load(std::sync::atomic::Ordering::Relaxed);
    format!(
        "# sqry Tool Reference\n\n{tools} tools for AST-based semantic code search. Unlike embedding search, sqry parses code structure.\n{TOOL_GUIDE_BODY}"
    )
}

const QUERY_SYNTAX_MD: &str = "\
# sqry Query Syntax

This syntax is for the **`query` parameter**. For the `filters` parameter, \
pass a JSON object instead (see _Query Predicates vs Structured Filters_ below).

Used in `semantic_search` and `hierarchical_search` `query` parameter.

## Fields

| Field | Values | Example |
|-------|--------|---------|
| `kind` | function, method, class, interface, trait, struct, enum, module, variable, constant, type, macro, import, export, component, service, endpoint, test | `kind:function` |
| `lang` | rust, python, javascript, typescript, java, go, cpp, c, ruby, php, kotlin, scala, swift, dart, lua, perl, elixir, haskell, zig, shell, sql, css, html, json, terraform, puppet, pulumi, groovy, r, vue, svelte, abap, apex, plsql, servicenow-xanadu, servicenow-xml | `lang:rust` |
| `vis` | public, private | `vis:public` |
| `name` | Any string or glob pattern | `name:parse*` |
| `file` | Path glob pattern | `file:src/lib.rs` |
| `returns` | Type name | `returns:Result` |
| `lines` | Numeric comparison | `lines>100` |

## Operators

| Op | Meaning | Example |
|----|---------|---------|
| `:` | Exact match / glob | `kind:function` |
| `~=` | Regex match | `name~=/^get_/` |
| `>` `<` `>=` `<=` | Numeric comparison | `lines>50` |

## Combinators

| Op | Example |
|----|---------|
| `AND` (default) | `kind:function lang:rust` |
| `OR` | `kind:function OR kind:method` |
| `NOT` | `NOT vis:private` |
| `()` | `(kind:function OR kind:method) AND lang:rust` |

## Examples

```
kind:function lang:rust vis:public
kind:class name:*Parser*
kind:function lines>100 NOT name:test*
(kind:function OR kind:method) lang:python file:src/**
name~=/^(get|set)_/ kind:method
```

## Query Predicates vs Structured Filters

There are two ways to constrain search results:

| Approach | Parameter | Syntax | Best for |
|----------|-----------|--------|----------|
| Query predicates | `query` | `\"lang:rust kind:function vis:public\"` | Complex boolean expressions with AND/OR/NOT, regex, globs |
| Structured filters | `filters` | `{\"language\":[\"rust\"],\"symbol_kind\":[\"function\"],\"visibility\":\"public\"}` | Simple pre-filtering by language, kind, or visibility |

**Side-by-side example:**

Query style (everything in `query`):
```
semantic_search query=\"kind:function lang:rust vis:public\"
```

Filter style (`query` + `filters`):
```
semantic_search query=\"kind:function\" filters={\"language\":[\"rust\"],\"visibility\":\"public\"}
```

Both can be combined: use `query` for complex expressions and `filters` for \
simple structured constraints. Filters are applied server-side **before** query \
evaluation (pre-filter), which can improve performance for large codebases.
";

const PATTERNS_MD: &str = "\
# Common Patterns

## Investigate a Symbol

```
1. get_definition symbol:\"MyFunction\"     # Where is it?
2. get_hover_info symbol:\"MyFunction\"     # Signature + docs
3. direct_callers symbol:\"MyFunction\"     # Who calls it?
4. direct_callees symbol:\"MyFunction\"     # What does it call?
5. get_references symbol:\"MyFunction\"     # All usages
```

## Impact Analysis (Before Refactoring)

```
1. dependency_impact symbol:\"OldApi\"      # What breaks?
2. direct_callers symbol:\"OldApi\"         # Immediate callers
3. trace_path from:\"UserHandler\" to:\"OldApi\"  # How is it reached?
4. subgraph symbols:[\"OldApi\"]            # Visual context
```

## Code Review (PR Analysis)

```
1. semantic_diff base:{ref:\"main\"} target:{ref:\"HEAD\"}  # What changed?
2. semantic_search query:\"name:changed_fn\" filters:{\"language\":[\"rust\"]}  # Narrow by lang
3. dependency_impact symbol:\"changed_fn\"   # Blast radius
4. find_cycles                              # New cycles introduced?
5. complexity_metrics target:\"changed.rs\"  # Complexity check
```

## Onboard to Codebase

```
1. get_graph_stats                         # Size and languages
2. get_insights                            # Health overview
3. semantic_search query:\"kind:module\"    # Top-level structure
4. list_files language:\"rust\"            # File inventory
5. hierarchical_search query:\"kind:class\" # Class hierarchy
```

## Find Dead Code

```
1. find_unused scope:\"public\"            # Unused public APIs
2. find_unused scope:\"function\"          # Unreachable functions
3. find_duplicates duplicate_type:\"body\"  # Copy-paste code
```

## Cross-Language Tracing

```
1. cross_language_edges from_lang:\"TypeScript\" to_lang:\"Python\"
2. trace_path from:\"fetchUsers\" to:\"get_users\"  # JS->Python
3. subgraph symbols:[\"fetchUsers\",\"get_users\"]  # Visual
```

## Search with Filters

Use `query` for complex expressions (AND/OR/NOT/regex). \
Use `filters` for simple language/kind/visibility pre-filtering.

Pure query (everything in `query`):
```
semantic_search query=\"kind:function lang:rust vis:public\"
```

With filters (`query` + `filters` object):
```
semantic_search query=\"kind:function\" filters:{\"language\":[\"rust\"],\"visibility\":\"public\"}
```

Combined (complex query + structured filters):
```
semantic_search query=\"name~=/^auth/ AND (kind:function OR kind:method)\" \
  filters:{\"language\":[\"typescript\"],\"visibility\":\"public\"}
```

## Export for Documentation

```
1. export_graph symbol:\"AuthModule\" format:\"mermaid\" max_depth:2
2. export_graph file_path:\"src/api.rs\" format:\"d2\" include:[\"calls\",\"imports\"]
```
";

const ARCHITECTURE_MD: &str = "\
# sqry Graph Architecture

## Storage: Arena + CSR

- **Arena allocation**: O(1) node/edge access via generational indices
- **CSR (Compressed Sparse Row)**: Cache-friendly edge traversal
- **String interning**: Deduplicates symbol names, saves 10-50 MB
- **Generational `NodeId`**: `{index, generation}` detects stale references

## Node Kinds (34)

Function, Method, Class, Interface, Trait, Module, Variable, Constant, Type, Struct, Enum, \
EnumVariant, Macro, Parameter, Property, CallSite, Import, Export, StyleRule, StyleAtRule, \
StyleVariable, Lifetime, Component, Service, Resource, Endpoint, Test, TypeParameter, \
Annotation, AnnotationValue, LambdaTarget, JavaModule, EnumConstant, Other

## Edge Kinds (38)

**Structural:** Defines, Contains
**References:** Calls{arg_count, is_async}, References, Imports{alias, is_wildcard}, \
Exports{kind, alias}, TypeOf{context, index, name}
**OOP:** Inherits, Implements
**Rust-specific:** LifetimeConstraint, TraitMethodBinding, MacroExpansion{expansion_kind}
**Cross-language:** FfiCall, HttpRequest, GrpcCall, WebAssemblyCall, DbQuery
**Database:** TableRead, TableWrite, TriggeredBy
**Extended:** MessageQueue, WebSocket, GraphQLOperation, ProcessExec, FileIpc, ProtocolCall
**JVM Classpath:** GenericBound, AnnotatedWith, AnnotationParam, LambdaCaptures, \
ModuleExports, ModuleRequires, ModuleOpens, ModuleProvides, TypeArgument, ExtensionReceiver, \
CompanionOf, SealedPermit

## Build Pipeline (5 passes)

| Pass | What | Output |
|------|------|--------|
| 1 | AST -> Nodes | Defines/Contains edges |
| 2 | Enrichment | Visibility, types, signatures |
| 3 | Intra-file | Calls, references within file |
| 4 | Cross-file | Import resolution via ExportMap |
| 5 | Cross-language | FFI + HTTP endpoint linking |

## Concurrency

- **CodeGraph**: `Arc`-wrapped, O(1) snapshot creation
- **ConcurrentCodeGraph**: `RwLock` + MVCC (single-writer, multi-reader)
- **GraphSnapshot**: Immutable view for queries

## Persistence

- **Location**: `.sqry/graph/snapshot.sqry`
- **Format**: V7 (postcard serialization, SHA-256 integrity verification)
- **CLI**: `sqry index` builds/saves, `sqry graph *` loads

## Language Support

37 plugins via `GraphBuilder` trait, including: Rust, Python, JavaScript, TypeScript, Java, Go, \
C/C++, C#, Ruby, PHP, Kotlin, Scala, Swift, Dart, Lua, Perl, Elixir, Haskell, Zig, Shell, SQL, \
CSS, HTML, JSON, Terraform, HCL, Puppet, Groovy, R, Vue, Svelte, SAP ABAP, Salesforce Apex, \
Oracle PL/SQL, ServiceNow Xanadu, ServiceNow XML
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_returns_all_resources() {
        let resources = list_resources();
        assert_eq!(resources.len(), 6);
        assert_eq!(resources[0].raw.uri, RESOURCE_TOOL_GUIDE);
        assert_eq!(resources[1].raw.uri, RESOURCE_QUERY_SYNTAX);
        assert_eq!(resources[2].raw.uri, RESOURCE_PATTERNS);
        assert_eq!(resources[3].raw.uri, RESOURCE_ARCHITECTURE);
        assert_eq!(resources[4].raw.uri, RESOURCE_MANIFEST);
        assert_eq!(resources[5].raw.uri, RESOURCE_CAPABILITY_MAP);
    }

    #[test]
    #[allow(clippy::match_same_arms)] // Arms separated for documentation clarity
    #[allow(clippy::match_wildcard_for_single_variants)] // Wildcard covers future variants
    fn read_known_resource_returns_content() {
        // Markdown resources
        for uri in [
            RESOURCE_TOOL_GUIDE,
            RESOURCE_QUERY_SYNTAX,
            RESOURCE_PATTERNS,
            RESOURCE_ARCHITECTURE,
        ] {
            let result = read_resource(uri);
            assert!(result.is_some(), "missing resource: {uri}");
            #[allow(clippy::match_same_arms)] // Arms separated for documentation clarity
            match result.unwrap() {
                ResourceContents::TextResourceContents {
                    text, mime_type, ..
                } => {
                    assert!(!text.is_empty(), "empty content: {uri}");
                    assert_eq!(mime_type.as_deref(), Some("text/markdown"));
                }
                #[allow(clippy::match_same_arms)] // Resource type arms intentionally separate
                _ => panic!("expected text resource for {uri}"),
            }
        }
    }

    #[test]
    #[allow(clippy::match_wildcard_for_single_variants)] // Wildcard covers future variants
    fn manifest_resource_returns_valid_json() {
        // Set a test tool count
        set_tool_count(34);
        let result = read_resource(RESOURCE_MANIFEST);
        assert!(result.is_some(), "manifest resource missing");
        match result.unwrap() {
            ResourceContents::TextResourceContents {
                text, mime_type, ..
            } => {
                assert_eq!(mime_type.as_deref(), Some("application/json"));
                let json: serde_json::Value =
                    serde_json::from_str(&text).expect("manifest is not valid JSON");
                assert_eq!(json["version"], env!("CARGO_PKG_VERSION"));
                assert_eq!(json["tools"], 34);
                assert_eq!(json["languages"]["total"], LANGUAGE_COUNT_TOTAL);
                assert_eq!(
                    json["languages"]["full_relation"],
                    LANGUAGE_COUNT_FULL_RELATION
                );
                assert_eq!(
                    json["languages"]["symbol_extraction"],
                    LANGUAGE_COUNT_SYMBOL_EXTRACTION
                );
                assert_eq!(json["snapshot_format"], SNAPSHOT_FORMAT_VERSION);
                assert_eq!(
                    json["defaults"]["redaction_preset"],
                    DEFAULT_REDACTION_PRESET
                );
                assert_eq!(
                    json["defaults"]["query_timeout_ms"],
                    DEFAULT_QUERY_TIMEOUT_MS
                );
                assert_eq!(
                    json["defaults"]["index_timeout_ms"],
                    DEFAULT_INDEX_TIMEOUT_MS
                );
            }
            _ => panic!("expected text resource for manifest"),
        }
    }

    #[test]
    #[allow(clippy::match_wildcard_for_single_variants)] // Wildcard covers future variants
    fn capability_map_contains_categories_and_tools() {
        // Set tool names matching the real registry
        set_tool_names(vec![
            "semantic_search".into(),
            "hierarchical_search".into(),
            "pattern_search".into(),
            "get_workspace_symbols".into(),
            "search_similar".into(),
            "get_definition".into(),
            "get_references".into(),
            "get_hover_info".into(),
            "get_document_symbols".into(),
            "relation_query".into(),
            "direct_callers".into(),
            "direct_callees".into(),
            "call_hierarchy".into(),
            "trace_path".into(),
            "dependency_impact".into(),
            "show_dependencies".into(),
            "subgraph".into(),
            "explain_code".into(),
            "find_cycles".into(),
            "is_node_in_cycle".into(),
            "find_unused".into(),
            "find_duplicates".into(),
            "complexity_metrics".into(),
            "semantic_diff".into(),
            "cross_language_edges".into(),
            "get_index_status".into(),
            "get_graph_stats".into(),
            "get_insights".into(),
            "rebuild_index".into(),
            "list_files".into(),
            "list_symbols".into(),
            "expand_cache_status".into(),
            "export_graph".into(),
            "sqry_ask".into(),
            "workspace_status".into(),
        ]);
        let result = read_resource(RESOURCE_CAPABILITY_MAP);
        assert!(result.is_some(), "capability-map resource missing");
        match result.unwrap() {
            ResourceContents::TextResourceContents {
                text, mime_type, ..
            } => {
                assert_eq!(mime_type.as_deref(), Some("text/markdown"));
                assert!(text.contains("Find a symbol"), "missing Search category");
                assert!(
                    text.contains("Navigate to it"),
                    "missing Navigation category"
                );
                assert!(
                    text.contains("Trace relationships"),
                    "missing Relationships category"
                );
                assert!(
                    text.contains("semantic_search"),
                    "missing semantic_search tool"
                );
                assert!(
                    text.contains("get_definition"),
                    "missing get_definition tool"
                );
                assert!(text.contains("find_cycles"), "missing find_cycles tool");
                assert!(
                    text.contains("sqry://docs/tool-guide"),
                    "missing tool-guide link"
                );
            }
            _ => panic!("expected text resource for capability-map"),
        }
    }

    #[test]
    fn read_unknown_resource_returns_none() {
        assert!(read_resource("sqry://docs/nonexistent").is_none());
        assert!(read_resource("https://example.com").is_none());
    }

    #[test]
    fn resources_have_descriptions() {
        for resource in list_resources() {
            assert!(
                resource.raw.description.is_some(),
                "missing description: {}",
                resource.raw.uri
            );
        }
    }

    /// Cross-validation: every tool name in the test registry must appear
    /// in both the tool-guide and capability-map resources.
    #[test]
    fn tool_guide_contains_all_registered_tools() {
        let tool_names = vec![
            "semantic_search",
            "hierarchical_search",
            "pattern_search",
            "get_workspace_symbols",
            "search_similar",
            "get_definition",
            "get_references",
            "get_hover_info",
            "get_document_symbols",
            "relation_query",
            "direct_callers",
            "direct_callees",
            "call_hierarchy",
            "trace_path",
            "dependency_impact",
            "show_dependencies",
            "subgraph",
            "explain_code",
            "find_cycles",
            "is_node_in_cycle",
            "find_unused",
            "find_duplicates",
            "complexity_metrics",
            "semantic_diff",
            "cross_language_edges",
            "get_index_status",
            "get_graph_stats",
            "get_insights",
            "rebuild_index",
            "list_files",
            "list_symbols",
            "expand_cache_status",
            "export_graph",
            "sqry_ask",
            "workspace_status",
        ];

        // Set up
        #[allow(clippy::cast_possible_truncation)] // Resource count fits in u32
        set_tool_count(tool_names.len() as u32);
        set_tool_names(tool_names.iter().map(|s| (*s).to_string()).collect());

        // Check tool-guide
        let guide = build_tool_guide();
        for name in &tool_names {
            assert!(guide.contains(name), "tool-guide missing tool: {name}");
        }

        // Check capability-map
        let cap_map = build_capability_map();
        for name in &tool_names {
            assert!(
                cap_map.contains(name),
                "capability-map missing tool: {name}"
            );
        }
    }

    /// Verify no hard-coded numeric counts remain in resource descriptions.
    #[test]
    fn no_hardcoded_counts_in_resource_descriptions() {
        let stale_patterns = [
            " tools ",
            " tools,",
            " tools.",
            " node kinds",
            " edge kinds",
            "33 ",
            "34 ",
            "35 ",
            "37 ",
        ];
        for resource in list_resources() {
            if let Some(desc) = &resource.raw.description {
                for pat in &stale_patterns {
                    assert!(
                        !desc.contains(pat),
                        "resource '{}' description may have hard-coded count (found '{}'): {}",
                        resource.raw.uri,
                        pat.trim(),
                        desc
                    );
                }
            }
        }
    }
}
