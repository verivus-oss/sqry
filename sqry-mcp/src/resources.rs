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

/// Build the list of available documentation resources.
pub fn list_resources() -> Vec<Resource> {
    vec![
        make_resource(
            RESOURCE_TOOL_GUIDE,
            "Tool Selection Guide",
            "Complete reference: 33 tools grouped by task with parameters and examples",
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
            "Arena+CSR storage, 22 node kinds, 20+ edge kinds, build pipeline, concurrency model",
        ),
    ]
}

/// Read a documentation resource by URI. Returns `None` for unknown URIs.
pub fn read_resource(uri: &str) -> Option<ResourceContents> {
    let content = match uri {
        RESOURCE_TOOL_GUIDE => TOOL_GUIDE_MD,
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

const TOOL_GUIDE_MD: &str = "\
# sqry Tool Reference

33 tools for AST-based semantic code search. Unlike embedding search, sqry parses code structure.

## Search

| Tool | Purpose | Key Params |
|------|---------|------------|
| `semantic_search` | Search by name/kind/visibility/language | query:str!, filters?, context_lines:int? |
| `hierarchical_search` | RAG-optimized search grouped by file/container | query:str!, max_files:int?, include_file_context:bool? |
| `pattern_search` | Find symbols by substring match | pattern:str! |
| `get_workspace_symbols` | Search symbols by name across workspace | query:str! |
| `sqry_ask` | Natural language to sqry query | query:str! |

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

## Export

| Tool | Purpose | Key Params |
|------|---------|------------|
| `export_graph` | Export subgraph as JSON/DOT/D2/Mermaid | symbol_name:str?, format:enum? |
";

const QUERY_SYNTAX_MD: &str = "\
# sqry Query Syntax

Used in `semantic_search` and `hierarchical_search` `query` parameter.

## Fields

| Field | Values | Example |
|-------|--------|---------|
| `kind` | function, method, class, interface, trait, struct, enum, module, variable, constant, type, macro, import, export, component, service, endpoint, test | `kind:function` |
| `lang` | rust, python, javascript, typescript, java, go, cpp, c, ruby, php, kotlin, scala, swift, dart, lua, perl, elixir, haskell, zig, shell, sql, css, html, terraform, puppet, groovy, r, vue, svelte, abap, apex, plsql, servicenow | `lang:rust` |
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
2. dependency_impact symbol:\"changed_fn\"   # Blast radius
3. find_cycles                              # New cycles introduced?
4. complexity_metrics target:\"changed.rs\"  # Complexity check
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

## Node Kinds (22)

Function, Method, Class, Interface, Trait, Module, Variable, Constant, Type, Struct, Enum, \
EnumVariant, Macro, CallSite, Import, Export, Component, Service, Resource, Endpoint, Test, Other

## Edge Kinds (20+)

**Structural:** Defines, Contains
**References:** Calls{arg_count, is_async}, References, Imports{alias, is_wildcard}, \
Exports{kind, alias}, TypeOf
**OOP:** Inherits, Implements
**Cross-language:** FfiCall, HttpRequest, GrpcCall, WebAssemblyCall, DbQuery
**Extended:** MessageQueue, WebSocket, GraphQLOperation, ProcessExec, FileIpc, ProtocolCall

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
- **Format**: bincode serialization
- **CLI**: `sqry index` builds/saves, `sqry graph *` loads

## Language Support

34 plugins via `GraphBuilder` trait, including: Rust, Python, JavaScript, TypeScript, Java, Go, \
C/C++, C#, Ruby, PHP, Kotlin, Scala, Swift, Dart, Lua, Perl, Elixir, Haskell, Zig, Shell, SQL, \
CSS, HTML, Terraform, HCL, Puppet, Groovy, R, Vue, Svelte, SAP ABAP, Salesforce Apex, Oracle \
PL/SQL, ServiceNow
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_returns_all_resources() {
        let resources = list_resources();
        assert_eq!(resources.len(), 4);
        assert_eq!(resources[0].raw.uri, RESOURCE_TOOL_GUIDE);
        assert_eq!(resources[1].raw.uri, RESOURCE_QUERY_SYNTAX);
        assert_eq!(resources[2].raw.uri, RESOURCE_PATTERNS);
        assert_eq!(resources[3].raw.uri, RESOURCE_ARCHITECTURE);
    }

    #[test]
    fn read_known_resource_returns_content() {
        for uri in [
            RESOURCE_TOOL_GUIDE,
            RESOURCE_QUERY_SYNTAX,
            RESOURCE_PATTERNS,
            RESOURCE_ARCHITECTURE,
        ] {
            let result = read_resource(uri);
            assert!(result.is_some(), "missing resource: {uri}");
            match result.unwrap() {
                ResourceContents::TextResourceContents {
                    text, mime_type, ..
                } => {
                    assert!(!text.is_empty(), "empty content: {uri}");
                    assert_eq!(mime_type.as_deref(), Some("text/markdown"));
                }
                _ => panic!("expected text resource for {uri}"),
            }
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
}
