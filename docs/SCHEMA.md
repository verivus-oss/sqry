# sqry Data Dictionary - by Verivus

This document defines the canonical types used across all sqry interfaces (CLI, LSP, MCP).
All types are exported from `sqry_core::schema` and should be the single source of truth.
Graph kinds (`NodeKind`, `EdgeKind`) are defined in `sqry-core/src/graph/unified/` and re-exported from the schema module.

**Version**: 27.0.4
**Last Updated**: 2026-07-04

---

## Table of Contents

1. [Graph Types](#graph-types)
   - [NodeKind](#nodekind)
   - [EdgeKind](#edgekind)
2. [Query Types](#query-types)
   - [RelationKind](#relationkind)
   - [Visibility](#visibility)
3. [Output Types](#output-types)
   - [OutputFormat](#outputformat)
4. [Analysis Types](#analysis-types)
   - [ChangeKind](#changekind)
   - [CycleKind](#cyclekind)
   - [DuplicateKind](#duplicatekind)
   - [UnusedScope](#unusedscope)
5. [Body-Shape Descriptor (V15)](#body-shape-descriptor-v15)
6. [Usage Guidelines](#usage-guidelines)

---

## Graph Types

### NodeKind

**Location**: `sqry-core/src/graph/unified/node/kind.rs`
**Serialization**: snake_case (`"function"`, `"call_site"`)

Categories of code symbols that can be represented as graph nodes.

| Variant | Description | Example |
|---------|-------------|---------|
| `function` | Standalone function | `fn main()` |
| `method` | Method belonging to a class/struct/trait | `impl Foo { fn bar() }` |
| `class` | Class definition (OOP languages) | `class MyClass {}` |
| `interface` | Interface definition | `interface IFoo {}` |
| `trait` | Trait definition (Rust, Scala) | `trait Iterator {}` |
| `module` | Module or namespace declaration | `mod utils;` |
| `variable` | Variable binding | `let x = 5;` |
| `constant` | Constant value | `const MAX: i32 = 100;` |
| `type` | Type alias or typedef | `type Result<T> = ...` |
| `struct` | Struct definition | `struct Point { x, y }` |
| `enum` | Enum definition | `enum Color { Red, Green }` |
| `enum_variant` | Enum variant | `Color::Red` |
| `macro` | Macro definition | `macro_rules! vec!` |
| `parameter` | Function parameter | `fn foo(x: i32)` |
| `property` | Class property or struct field | `self.name` |
| `call_site` | Location where a call occurs | For fine-grained call graphs |
| `import` | Import statement | `use std::io;` |
| `export` | Export statement | `export { foo }` |
| `style_rule` | CSS selector block | `.header { }` |
| `style_at_rule` | CSS at-rule | `@media screen` |
| `style_variable` | CSS variable | `--primary-color` |
| `lifetime` | Rust lifetime parameter | `'a`, `'static` |
| `component` | UI component | React/Vue/Angular components |
| `service` | Service class/function | Dependency injection services |
| `resource` | REST/GraphQL resource | API endpoints |
| `endpoint` | API endpoint handler | Route handlers |
| `test` | Test function | `#[test] fn test_foo()` |
| `other` | Custom/plugin-specific | Extensibility fallback |

**Helper Methods**:
- `is_callable()` → `true` for Function, Method, Macro
- `is_type_definition()` → `true` for Class, Interface, Trait, Struct, Enum, Type
- `is_container()` → `true` for Class, Interface, Trait, Struct, Module, Enum, StyleRule, StyleAtRule
- `is_boundary()` → `true` for Import, Export

---

### EdgeKind

**Location**: `sqry-core/src/graph/unified/edge/kind.rs`
**Serialization**: snake_case with metadata (`{"calls": {"argument_count": 2, "is_async": false}}`)

Relationship types between graph nodes.

#### Structural Edges

| Variant | Description |
|---------|-------------|
| `defines` | Symbol defines another (module → function) |
| `contains` | Container contains another (class → method) |

#### Reference Edges

| Variant | Fields | Description |
|---------|--------|-------------|
| `calls` | `argument_count: u8`, `is_async: bool` | Function/method call |
| `references` | - | Read access to a symbol |
| `imports` | `alias: Option<StringId>`, `is_wildcard: bool` | Import statement |
| `exports` | `kind: ExportKind`, `alias: Option<StringId>` | Export statement |
| `type_of` | `context: Option<TypeOfContext>`, `index: Option<u16>`, `name: Option<StringId>` | Type reference (return type, parameter type) |

**ExportKind values**: `direct`, `reexport`, `default`, `namespace`

#### OOP Edges

| Variant | Description |
|---------|-------------|
| `inherits` | Class extends another |
| `implements` | Class/struct implements interface/trait |

#### Rust-Specific Edges

| Variant | Fields | Description |
|---------|--------|-------------|
| `lifetime_constraint` | `constraint_kind: LifetimeConstraintKind` | `'a: 'b` relationships |
| `trait_method_binding` | `trait_name`, `impl_type`, `is_ambiguous` | Trait method resolution |
| `macro_expansion` | `expansion_kind`, `is_verified` | Macro expansion |

**LifetimeConstraintKind values**: `outlives`, `type_bound`, `reference`, `static`, `higher_ranked`, `trait_object`, `impl_trait`, `elided`

#### Cross-Language Edges

| Variant | Fields | Description |
|---------|--------|-------------|
| `ffi_call` | `convention: FfiConvention` | FFI call |
| `http_request` | `method: HttpMethod`, `url` | HTTP request |
| `grpc_call` | `service`, `method` | gRPC call |
| `web_assembly_call` | - | WASM call |
| `db_query` | `query_type`, `table` | Database query |
| `table_read` | `table_name`, `schema` | SQL table read |
| `table_write` | `table_name`, `schema`, `operation` | SQL table write |
| `triggered_by` | `trigger_name`, `schema` | Database trigger |
| `message_queue` | `protocol: MqProtocol`, `topic` | Async messaging |
| `web_socket` | `event` | WebSocket event |
| `graphql_operation` | `operation` | GraphQL query/mutation |
| `process_exec` | `command` | Process spawn |
| `file_ipc` | `path_pattern` | File-based IPC |
| `protocol_call` | `protocol`, `metadata` | Generic protocol |

**Helper Methods**:
- `is_call()` → `true` for Calls, FfiCall, HttpRequest, GrpcCall, WebAssemblyCall
- `is_structural()` → `true` for Defines, Contains
- `is_type_relation()` → `true` for Inherits, Implements, TypeOf
- `is_cross_boundary()` → `true` for all cross-language edges
- `is_rust_specific()` → `true` for LifetimeConstraint, TraitMethodBinding, MacroExpansion

---

## Query Types

### RelationKind

**Location**: `sqry-core/src/schema/relation.rs`
**Serialization**: lowercase (`"callers"`, `"callees"`)

Types of symbol relationships for `relation_query` operations.

| Variant | Description | Edge Traversal |
|---------|-------------|----------------|
| `callers` | Symbols that call the target | Incoming `Calls` edges |
| `callees` | Symbols called by the target | Outgoing `Calls` edges |
| `imports` | Symbols imported by target | `Imports` edges |
| `exports` | Symbols exported by target | `Exports` edges |
| `returns` | Return type relationships | `TypeOf` edges |

**Helper Methods**:
- `is_call_relation()` → `true` for Callers, Callees
- `is_boundary_relation()` → `true` for Imports, Exports

---

### Visibility

**Location**: `sqry-core/src/schema/visibility.rs`
**Serialization**: lowercase (`"public"`, `"private"`)

Symbol visibility levels for filtering search results.

| Variant | Description | Language Examples |
|---------|-------------|-------------------|
| `public` | Exported, accessible outside module | `pub`, `export`, `public` |
| `private` | Internal to defining module | no modifier, `private`, `internal` |

---

## Output Types

### OutputFormat

**Location**: `sqry-core/src/schema/format.rs`
**Serialization**: lowercase (`"json"`, `"mermaid"`)

Output formats for graph exports and visualizations.

| Variant | Extension | Description | Render Command |
|---------|-----------|-------------|----------------|
| `json` | `.json` | Structured JSON (default) | N/A |
| `dot` | `.dot` | Graphviz DOT format | `dot -Tpng graph.dot -o graph.png` |
| `d2` | `.d2` | D2 diagram language | `d2 graph.d2 graph.svg` |
| `mermaid` | `.mmd` | Mermaid diagram syntax | Renders in GitHub/GitLab |

**Helper Methods**:
- `is_text_format()` → `true` (all current formats)
- `is_diagram_format()` → `true` for Dot, D2, Mermaid

---

## Analysis Types

### ChangeKind

**Location**: `sqry-core/src/schema/change.rs`
**Serialization**: snake_case (`"added"`, `"signature_changed"`)

Types of changes detected by `semantic_diff`.

| Variant | Description |
|---------|-------------|
| `added` | Symbol exists in target, not in base |
| `removed` | Symbol exists in base, not in target |
| `modified` | Implementation changed, signature same |
| `renamed` | Same implementation, different name |
| `signature_changed` | Parameters, return type, etc. changed |

**Helper Methods**:
- `is_structural()` → `true` for Added, Removed
- `is_content_change()` → `true` for Modified, Renamed, SignatureChanged
- `affects_api()` → `true` for Added, Removed, Renamed, SignatureChanged

---

### CycleKind

**Location**: `sqry-core/src/schema/cycle.rs`
**Serialization**: lowercase (`"calls"`, `"imports"`)

Types of cycles to detect in code graphs.

| Variant | Description |
|---------|-------------|
| `calls` | Call cycles (A calls B, B calls A) |
| `imports` | Import cycles (circular dependencies) |
| `modules` | Module-level dependency cycles |

---

### DuplicateKind

**Location**: `sqry-core/src/schema/duplicate.rs`
**Serialization**: lowercase (`"body"`, `"signature"`)

Types of duplicates to detect in code.

| Variant | Description |
|---------|-------------|
| `body` | Function/method body duplicates |
| `signature` | Function signature duplicates |
| `struct` | Struct/class definition duplicates |

---

### UnusedScope

**Location**: `sqry-core/src/schema/unused.rs`
**Serialization**: lowercase (`"public"`, `"all"`)

Scopes for unused symbol detection.

| Variant | Description |
|---------|-------------|
| `public` | Unused public/exported symbols |
| `private` | Unused private/internal symbols |
| `function` | Unused functions and methods |
| `struct` | Unused structs and classes |
| `all` | All unused symbols (default) |

**Helper Methods**:
- `is_visibility_filter()` → `true` for Public, Private
- `is_kind_filter()` → `true` for Function, Struct

---

## Body-Shape Descriptor (V15)

**Location**: `sqry-core/src/graph/unified/storage/shape.rs` (types),
`sqry-core/src/graph/unified/build/shape.rs` (walker + `CfBucket`).

An identifier-blind structural fingerprint of one Function/Method body, computed
in the build seam alongside `body_hash` and stored in a `NodeId`-keyed side table
on `NodeMetadataStore` (`shape_descriptors`). It is a deterministic function of the
AST (grammar productions + arities), never of the source text, so it is invariant
under renaming and reformatting. It is distinct from `body_hash` (exact-bytes
duplicate detection), which is unchanged.

### ShapeDescriptor

| Field | Type | Description |
|-------|------|-------------|
| `cf_histogram` | `[u16; 15]` | Canonical control-flow bucket counts (see `CfBucket`) |
| `signature_shape` | `SignatureShape` | Structural arities (positional / keyword-only), varargs + return-annotation flags |
| `callee_shape` | `CalleeShape` | Bucketed callee fan-out; `Unresolved` this effort (gated on call-resolution-completeness), never silently zeroed |
| `shape_hash` | `ShapeHash128` | Rename-invariant dual-seeded structural hash (`{high, low}`, 32-char hex); NOT `body_hash` |
| `minhash` | `[u32; 64]` | MinHash over identifier-erased Weisfeiler-Lehman labels, for LSH banding |
| `flags` | `ShapeFlags` | Explicit `unhashable` (<4-token body) / `truncated` (over walker budget) markers, never a silently-zeroed descriptor |

**CfBucket** (`#[repr(u8)]`, frozen 0..14, additive-only): `Branch`, `Loop`,
`Match`, `Try`, `Catch`, `Throw`, `Return`, `Call`, `Closure`, `Assign`, `Await`,
`Yield`, `Resource`, `Jump`, `Comprehension`. Shared by every plugin's
`ShapeMapping`, so histograms are comparable across languages (AC-6).

**CalleeShape**: `Unresolved` (default, this effort) | `Resolved { count, degree_buckets }`.

### SHAPE_SCHEMA_VERSION

`SHAPE_SCHEMA_VERSION = 1`, `CF_BUCKET_COUNT = 15`, `MINHASH_LANES = 64` are
on-disk-affecting constants: changing the `SQRYSHP0`/`SQRYSHP1` seeds, the
`CfBucket` order, or the lane count bumps `SHAPE_SCHEMA_VERSION` (a sibling
discipline to `INDEX_SCHEMA_VERSION`).

### Persistence (snapshot V15)

This feature owns the `SQRY_GRAPH_V14 -> V15` bump. Descriptors ride a dedicated
`GraphSnapshotDataV15` envelope slot (`shape_descriptors_table`), reattached on
load via `set_shape_descriptors`. They do NOT ride the `NodeMetadataStore` custom
serde (which carries only `entries`), matching the V12 framework-route /
dispatch-table joint-stub precedent. A V14 snapshot loads under the V15 loader with
an empty side table (inline `upconvert_v14_to_v15`, no forced rebuild); descriptors
populate on the next `sqry index --force` (AC-8).

---

## Usage Guidelines

### For Interface Packages (LSP, MCP)

Import canonical types from `sqry_core::schema`:

```rust
use sqry_core::schema::{
    RelationKind, Visibility, OutputFormat,
    ChangeKind, CycleKind, DuplicateKind, UnusedScope,
    NodeKind, EdgeKind,
};
```

### For JSON Schema Generation (MCP)

Create thin wrappers with schemars if needed:

```rust
use schemars::JsonSchema;
use sqry_core::schema::RelationKind;

#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RelationTypeParam(RelationKind);
```

### For Protocol-Specific Serialization

If a protocol requires different serialization, create a newtype with custom serde attributes:

```rust
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub struct ProtocolRelationKind(RelationKind);
```

### Adding New Types

1. Create the enum in `sqry-core/src/schema/<name>.rs`
2. Add `pub mod <name>;` to `sqry-core/src/schema/mod.rs`
3. Add `pub use <name>::<Type>;` to the re-exports
4. Update this documentation
5. Update interface packages to use the canonical type

---

## Version History

| Version | Date | Changes |
|---------|------|---------|
| 27.0.4 | 2026-06-25 | Document the body-shape descriptor types + snapshot V15 (`ShapeDescriptor`, `CfBucket`, `SHAPE_SCHEMA_VERSION`) |
| 27.0.4 | 2026-03-10 | Bump schema doc version to match current sqry release |
| 4.9.3 | 2026-03-03 | Align version with sqry release, verify accuracy |
| 2.10.0 | 2026-01-18 | Initial schema module creation |
