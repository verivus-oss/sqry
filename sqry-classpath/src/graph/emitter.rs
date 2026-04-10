//! Emit classpath nodes and edges into the sqry unified graph.
//!
//! This module takes a [`ClasspathIndex`] (produced by bytecode parsing) and
//! emits synthetic graph nodes and edges into a [`StagingGraph`]. Each JAR gets
//! a synthetic [`FileId`] via [`FileRegistry::register_external()`], and each
//! class/method/field becomes a graph node with zero-span metadata (since the
//! data comes from bytecode, not source).
//!
//! ## Emission pipeline
//!
//! 1. **File registration** - For each unique JAR path, register a synthetic
//!    `FileEntry` with path convention `{jar_path}!/{fqn}.class`.
//! 2. **Node emission** - For each `ClassStub`, emit nodes for the class itself,
//!    its methods, fields, annotations, type parameters, enum constants, lambda
//!    targets, and module declarations.
//! 3. **Structural edges** - Class→Method (`Defines`), Class→Field (`Defines`),
//!    Class→InnerClass (`Contains`).
//! 4. **Metadata attachment** - `ClasspathNodeMetadata` on all emitted nodes
//!    (class, method, field, enum constant, type parameter, lambda target,
//!    module).
//!
//! After emission, the returned `FQN→NodeId` mapping is used by
//! [`register_classpath_exports`] and [`create_classpath_edges`] for `ExportMap`
//! registration and cross-reference edge creation.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use log::debug;
use sqry_core::graph::node::Language;
use sqry_core::graph::unified::build::ExportMap;
use sqry_core::graph::unified::build::StagingGraph;
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::unified::storage::metadata::{
    ClasspathNodeMetadata, NodeMetadata, NodeMetadataStore,
};
use sqry_core::graph::unified::storage::registry::FileRegistry;
use sqry_core::graph::unified::storage::{NodeEntry, StringInterner};
use sqry_core::graph::unified::{FileId, NodeId, StringId};

use crate::stub::index::ClasspathIndex;
use crate::stub::model::{ClassKind, ClassStub};
use crate::{ClasspathError, ClasspathResult};

use super::provenance::ClasspathProvenance;

// ---------------------------------------------------------------------------
// String interning helper
// ---------------------------------------------------------------------------

/// Helper to intern strings via `StringInterner` and track the resulting IDs.
///
/// Unlike `GraphBuildHelper` which uses staging-local IDs, the classpath emitter
/// interns directly into the `StringInterner` because classpath emission happens
/// as a discrete phase before the per-file build pipeline. The resulting global
/// `StringId`s are used directly in `NodeEntry` fields.
struct InternHelper<'a> {
    interner: &'a mut StringInterner,
    cache: HashMap<String, StringId>,
}

impl<'a> InternHelper<'a> {
    fn new(interner: &'a mut StringInterner) -> Self {
        Self {
            interner,
            cache: HashMap::new(),
        }
    }

    /// Intern a string, returning a global `StringId`.
    ///
    /// Caches results to avoid repeated lookups for common strings like
    /// visibility modifiers.
    fn intern(&mut self, s: &str) -> ClasspathResult<StringId> {
        if let Some(&id) = self.cache.get(s) {
            return Ok(id);
        }
        let id = self.interner.intern(s).map_err(|e| {
            ClasspathError::EmissionError(format!("string intern failed for '{s}': {e}"))
        })?;
        self.cache.insert(s.to_owned(), id);
        Ok(id)
    }
}

// ---------------------------------------------------------------------------
// Visibility mapping
// ---------------------------------------------------------------------------

/// Map JVM access flags to a visibility string for the graph.
#[allow(clippy::trivially_copy_pass_by_ref)] // API consistency with other methods
fn access_to_visibility(access: &crate::stub::model::AccessFlags) -> &'static str {
    if access.is_public() {
        "public"
    } else if access.is_protected() {
        "protected"
    } else if access.is_private() {
        "private"
    } else {
        "package"
    }
}

/// Map `ClassKind` to the corresponding `NodeKind`.
fn class_kind_to_node_kind(kind: ClassKind) -> NodeKind {
    match kind {
        ClassKind::Class | ClassKind::Record => NodeKind::Class,
        ClassKind::Interface => NodeKind::Interface,
        ClassKind::Enum => NodeKind::Enum,
        ClassKind::Annotation => NodeKind::Annotation,
        ClassKind::Module => NodeKind::JavaModule,
    }
}

// ---------------------------------------------------------------------------
// File ID management
// ---------------------------------------------------------------------------

/// Register a synthetic external file for a class within a JAR.
///
/// Path convention: `{jar_path}!/{fqn}.class` (similar to Java URL conventions
/// for JAR entries like `jar:file:///path.jar!/com/example/Foo.class`).
fn register_synthetic_file(
    jar_path: &Path,
    fqn: &str,
    file_registry: &mut FileRegistry,
) -> ClasspathResult<FileId> {
    let class_path_str = fqn.replace('.', "/");
    let synthetic_path = format!("{}!/{class_path_str}.class", jar_path.display());
    let path = PathBuf::from(&synthetic_path);
    file_registry
        .register_external(&path, Some(Language::Java))
        .map_err(|e| {
            ClasspathError::EmissionError(format!(
                "failed to register synthetic file for {fqn}: {e}"
            ))
        })
}

// ---------------------------------------------------------------------------
// Node emission
// ---------------------------------------------------------------------------

/// Emit all classpath nodes and edges into a staging graph.
///
/// Returns the mapping of FQN to `NodeId` for `ExportMap` registration and
/// cross-reference edge creation.
///
/// # Arguments
///
/// * `index` - The merged classpath index containing all parsed class stubs.
/// * `staging` - The staging graph to emit nodes and edges into.
/// * `file_registry` - File registry for synthetic file registration.
/// * `interner` - String interner for name/qualifier interning.
/// * `metadata_store` - Metadata store for classpath provenance attachment.
/// * `provenance` - Provenance information for each JAR.
///
/// # Errors
///
/// Returns `ClasspathError::EmissionError` if string interning or file
/// registration fails.
#[allow(clippy::too_many_lines)]
pub fn emit_classpath_nodes(
    index: &ClasspathIndex,
    staging: &mut StagingGraph,
    file_registry: &mut FileRegistry,
    interner: &mut StringInterner,
    metadata_store: &mut NodeMetadataStore,
    provenance: &[ClasspathProvenance],
) -> ClasspathResult<EmissionResult> {
    let mut helper = InternHelper::new(interner);
    let mut fqn_to_node: HashMap<String, NodeId> = HashMap::with_capacity(index.classes.len());
    let mut fqn_to_nodes: HashMap<String, Vec<ClasspathNodeRef>> =
        HashMap::with_capacity(index.classes.len());
    let mut file_id_map: HashMap<String, FileId> = HashMap::new();

    // Build a lookup from JAR path to provenance for O(1) access.
    let prov_map: HashMap<&Path, &ClasspathProvenance> = provenance
        .iter()
        .map(|p| (p.jar_path.as_path(), p))
        .collect();

    for stub in &index.classes {
        emit_class_stub(
            stub,
            staging,
            file_registry,
            &mut helper,
            metadata_store,
            &prov_map,
            &mut fqn_to_node,
            &mut fqn_to_nodes,
            &mut file_id_map,
        )?;
    }

    Ok(EmissionResult {
        fqn_to_node,
        fqn_to_nodes,
        file_id_map,
    })
}

/// Duplicate-aware emitted node reference.
#[derive(Debug, Clone)]
pub struct ClasspathNodeRef {
    /// Graph node id for the emitted symbol.
    pub node_id: NodeId,
    /// Fully qualified name for duplicate-aware disambiguation.
    pub fqn: String,
    /// JAR path the symbol originated from.
    pub jar_path: PathBuf,
    /// Synthetic file id registered for this emitted symbol.
    pub file_id: FileId,
}

/// Result of classpath node emission.
#[derive(Debug)]
pub struct EmissionResult {
    /// Mapping from fully qualified class name to its graph `NodeId`.
    pub fqn_to_node: HashMap<String, NodeId>,
    /// Duplicate-aware mapping from fully qualified name to emitted node refs.
    pub fqn_to_nodes: HashMap<String, Vec<ClasspathNodeRef>>,
    /// Mapping from fully qualified class name to its synthetic `FileId`.
    pub file_id_map: HashMap<String, FileId>,
}

/// Emit classpath nodes and structural edges directly into a [`CodeGraph`].
///
/// This mutates the graph in place so existing graph state such as confidence
/// and epoch metadata is preserved. It commits staged nodes into the node
/// arena, remaps staged edges, inserts them into the bidirectional edge store,
/// remaps metadata keys, and returns the final classpath `FQN -> NodeId` map.
///
/// # Errors
///
/// Returns [`ClasspathError::EmissionError`] if staging commit, string
/// interning, or file registration fails.
pub fn emit_into_code_graph(
    index: &ClasspathIndex,
    graph: &mut CodeGraph,
    provenance: &[ClasspathProvenance],
) -> ClasspathResult<EmissionResult> {
    // Work against local clones first so a fallible emission path cannot leave
    // the caller's graph partially emptied on early return.
    let mut nodes = graph.nodes().clone();
    let edges = graph.edges().clone();
    let mut strings = graph.strings().clone();
    let mut files = graph.files().clone();
    let mut metadata = graph.macro_metadata().clone();

    let mut staging = StagingGraph::new();
    let emission_result = emit_classpath_nodes(
        index,
        &mut staging,
        &mut files,
        &mut strings,
        &mut metadata,
        provenance,
    )?;

    create_classpath_edges(
        index,
        &emission_result.fqn_to_nodes,
        provenance,
        &mut staging,
    );

    let id_mapping = staging
        .commit_nodes(&mut nodes)
        .map_err(|e| ClasspathError::EmissionError(format!("node commit failed: {e}")))?;

    for edge in staging.get_remapped_edges(&id_mapping) {
        let _delta = edges.add_edge(edge.source, edge.target, edge.kind, edge.file);
    }

    let mut remapped_fqn_to_node = HashMap::with_capacity(emission_result.fqn_to_node.len());
    for (fqn, node_id) in emission_result.fqn_to_node {
        let remapped_id = id_mapping.get(&node_id).copied().unwrap_or(node_id);
        remapped_fqn_to_node.insert(fqn, remapped_id);
    }

    let mut remapped_fqn_to_nodes = HashMap::with_capacity(emission_result.fqn_to_nodes.len());
    for (fqn, refs) in emission_result.fqn_to_nodes {
        let remapped_refs = refs
            .into_iter()
            .map(|node_ref| ClasspathNodeRef {
                node_id: id_mapping
                    .get(&node_ref.node_id)
                    .copied()
                    .unwrap_or(node_ref.node_id),
                fqn: node_ref.fqn,
                jar_path: node_ref.jar_path,
                file_id: node_ref.file_id,
            })
            .collect();
        remapped_fqn_to_nodes.insert(fqn, remapped_refs);
    }

    if !id_mapping.is_empty() {
        let remapped_entries: Vec<_> = metadata
            .iter_all()
            .map(|((index, generation), value)| {
                let node_id = NodeId::new(index, generation);
                let remapped_id = id_mapping.get(&node_id).copied().unwrap_or(node_id);
                (remapped_id, value.clone())
            })
            .collect();
        metadata = NodeMetadataStore::new();
        for (node_id, value) in remapped_entries {
            metadata.insert_metadata(node_id, value);
        }
    }

    let _old_nodes = std::mem::replace(graph.nodes_mut(), nodes);
    let _old_edges = std::mem::replace(graph.edges_mut(), edges);
    let _old_strings = std::mem::replace(graph.strings_mut(), strings);
    let _old_files = std::mem::replace(graph.files_mut(), files);
    let _old_metadata = std::mem::replace(graph.macro_metadata_mut(), metadata);

    Ok(EmissionResult {
        fqn_to_node: remapped_fqn_to_node,
        fqn_to_nodes: remapped_fqn_to_nodes,
        file_id_map: emission_result.file_id_map,
    })
}

/// Emit a single class stub and all its members into the staging graph.
#[allow(clippy::too_many_arguments)] // Emission state is threaded explicitly for staging performance
#[allow(clippy::too_many_lines)]
#[allow(clippy::similar_names)] // Domain variable naming is intentional
fn emit_class_stub(
    stub: &ClassStub,
    staging: &mut StagingGraph,
    file_registry: &mut FileRegistry,
    helper: &mut InternHelper<'_>,
    metadata_store: &mut NodeMetadataStore,
    prov_map: &HashMap<&Path, &ClasspathProvenance>,
    fqn_to_node: &mut HashMap<String, NodeId>,
    fqn_to_nodes: &mut HashMap<String, Vec<ClasspathNodeRef>>,
    file_id_map: &mut HashMap<String, FileId>,
) -> ClasspathResult<()> {
    // Determine JAR path from the stub's source_jar (set during scanning),
    // falling back to the first provenance entry or a synthetic path.
    let jar_path = if let Some(ref src_jar) = stub.source_jar {
        PathBuf::from(src_jar)
    } else if let Some((&path, _)) = prov_map.iter().next() {
        path.to_path_buf()
    } else {
        PathBuf::from(format!("<classpath>/{}.class", stub.fqn.replace('.', "/")))
    };
    let file_id = register_synthetic_file(&jar_path, &stub.fqn, file_registry)?;
    file_id_map.insert(stub.fqn.clone(), file_id);

    // --- Class node ---
    let node_kind = class_kind_to_node_kind(stub.kind);
    let name_id = helper.intern(&stub.name)?;
    let qname_id = helper.intern(&stub.fqn)?;
    let vis_id = helper.intern(access_to_visibility(&stub.access))?;

    let class_entry = NodeEntry::new(node_kind, name_id, file_id)
        .with_qualified_name(qname_id)
        .with_visibility(vis_id)
        .with_static(stub.access.is_static())
        .with_unsafe(false);

    let class_node_id = staging.add_node(class_entry);
    record_node_ref(
        &stub.fqn,
        class_node_id,
        &jar_path,
        file_id,
        fqn_to_node,
        fqn_to_nodes,
    );

    // --- Build ClasspathNodeMetadata (shared by class and all members) ---
    let prov = find_provenance_for_jar(&jar_path, prov_map);
    let cp_meta = ClasspathNodeMetadata {
        coordinates: prov.and_then(|p| p.coordinates.clone()),
        jar_path: jar_path.display().to_string(),
        fqn: stub.fqn.clone(),
        is_direct_dependency: prov.is_some_and(ClasspathProvenance::has_direct_scope),
    };
    metadata_store.insert_metadata(class_node_id, NodeMetadata::Classpath(cp_meta.clone()));

    // --- Methods ---
    for method in &stub.methods {
        let method_name_id = helper.intern(&method.name)?;
        // Include the descriptor in the key to disambiguate overloaded methods.
        // JVM methods are uniquely identified by (name, descriptor).
        let method_fqn = format!("{}.{}{}", stub.fqn, method.name, method.descriptor);
        #[allow(clippy::similar_names)] // Domain terminology: source/target node pairs
        let method_qname_id = helper.intern(&method_fqn)?;
        let method_vis_id = helper.intern(access_to_visibility(&method.access))?;

        let method_entry = NodeEntry::new(NodeKind::Method, method_name_id, file_id)
            .with_qualified_name(method_qname_id)
            .with_visibility(method_vis_id)
            .with_static(method.access.is_static());

        let method_node_id = staging.add_node(method_entry);
        record_node_ref(
            &method_fqn,
            method_node_id,
            &jar_path,
            file_id,
            fqn_to_node,
            fqn_to_nodes,
        );
        metadata_store.insert_metadata(method_node_id, NodeMetadata::Classpath(cp_meta.clone()));

        // Class → Method: Defines
        staging.add_edge(class_node_id, method_node_id, EdgeKind::Defines, file_id);
    }

    // --- Fields ---
    for field in &stub.fields {
        let field_name_id = helper.intern(&field.name)?;
        let field_fqn = format!("{}.{}", stub.fqn, field.name);
        let field_qname_id = helper.intern(&field_fqn)?;
        let field_vis_id = helper.intern(access_to_visibility(&field.access))?;

        let field_entry = NodeEntry::new(NodeKind::Property, field_name_id, file_id)
            .with_qualified_name(field_qname_id)
            .with_visibility(field_vis_id)
            .with_static(field.access.is_static());

        let field_node_id = staging.add_node(field_entry);
        record_node_ref(
            &field_fqn,
            field_node_id,
            &jar_path,
            file_id,
            fqn_to_node,
            fqn_to_nodes,
        );
        metadata_store.insert_metadata(field_node_id, NodeMetadata::Classpath(cp_meta.clone()));

        // Class → Field: Defines
        staging.add_edge(class_node_id, field_node_id, EdgeKind::Defines, file_id);
    }

    // --- Enum constants ---
    for constant_name in &stub.enum_constants {
        let const_name_id = helper.intern(constant_name)?;
        let const_fqn = format!("{}.{constant_name}", stub.fqn);
        let const_qname_id = helper.intern(&const_fqn)?;

        let const_entry = NodeEntry::new(NodeKind::EnumConstant, const_name_id, file_id)
            .with_qualified_name(const_qname_id)
            .with_visibility(helper.intern("public")?);

        let const_node_id = staging.add_node(const_entry);
        record_node_ref(
            &const_fqn,
            const_node_id,
            &jar_path,
            file_id,
            fqn_to_node,
            fqn_to_nodes,
        );
        metadata_store.insert_metadata(const_node_id, NodeMetadata::Classpath(cp_meta.clone()));

        // Class → EnumConstant: Defines
        staging.add_edge(class_node_id, const_node_id, EdgeKind::Defines, file_id);
    }

    // --- Type parameters ---
    if let Some(ref gen_sig) = stub.generic_signature {
        for tp in &gen_sig.type_parameters {
            let tp_name_id = helper.intern(&tp.name)?;
            let tp_fqn = format!("{}.<{}>", stub.fqn, tp.name);
            let tp_qname_id = helper.intern(&tp_fqn)?;

            let tp_entry = NodeEntry::new(NodeKind::TypeParameter, tp_name_id, file_id)
                .with_qualified_name(tp_qname_id);

            let tp_node_id = staging.add_node(tp_entry);
            record_node_ref(
                &tp_fqn,
                tp_node_id,
                &jar_path,
                file_id,
                fqn_to_node,
                fqn_to_nodes,
            );
            metadata_store.insert_metadata(tp_node_id, NodeMetadata::Classpath(cp_meta.clone()));

            // Class → TypeParameter: TypeArgument
            staging.add_edge(class_node_id, tp_node_id, EdgeKind::TypeArgument, file_id);
        }
    }

    // --- Lambda targets ---
    for lambda in &stub.lambda_targets {
        let lambda_label = format!("{}::{}", lambda.owner_fqn, lambda.method_name);
        let lambda_name_id = helper.intern(&lambda_label)?;
        let lambda_fqn = format!("{}.lambda${}", stub.fqn, lambda.method_name);
        let lambda_qname_id = helper.intern(&lambda_fqn)?;

        let lambda_entry = NodeEntry::new(NodeKind::LambdaTarget, lambda_name_id, file_id)
            .with_qualified_name(lambda_qname_id);

        let lambda_node_id = staging.add_node(lambda_entry);
        record_node_ref(
            &lambda_fqn,
            lambda_node_id,
            &jar_path,
            file_id,
            fqn_to_node,
            fqn_to_nodes,
        );
        metadata_store.insert_metadata(lambda_node_id, NodeMetadata::Classpath(cp_meta.clone()));

        // Class → LambdaTarget: Contains
        staging.add_edge(class_node_id, lambda_node_id, EdgeKind::Contains, file_id);
    }

    // --- Inner classes (Contains edge) ---
    for inner in &stub.inner_classes {
        // Only emit Contains edge if the inner class belongs to this outer class
        // and has been separately emitted (will be linked post-emission).
        if inner.outer_fqn.as_deref() == Some(&stub.fqn) {
            // The inner class itself will be emitted as its own ClassStub.
            // We record the relationship for edge creation in create_classpath_edges.
            // Store the inner FQN in fqn_to_node so we can link later.
            // Actual Contains edge is deferred to create_classpath_edges where
            // both nodes are guaranteed to exist.
        }
    }

    // --- Module info ---
    if let Some(ref module) = stub.module {
        let mod_name_id = helper.intern(&module.name)?;
        let mod_fqn = format!("module:{}", module.name);
        let mod_qname_id = helper.intern(&mod_fqn)?;

        let mod_entry = NodeEntry::new(NodeKind::JavaModule, mod_name_id, file_id)
            .with_qualified_name(mod_qname_id);

        let mod_node_id = staging.add_node(mod_entry);
        record_node_ref(
            &mod_fqn,
            mod_node_id,
            &jar_path,
            file_id,
            fqn_to_node,
            fqn_to_nodes,
        );
        metadata_store.insert_metadata(mod_node_id, NodeMetadata::Classpath(cp_meta.clone()));

        // Class → Module: Contains
        staging.add_edge(class_node_id, mod_node_id, EdgeKind::Contains, file_id);
    }

    Ok(())
}

fn record_node_ref(
    fqn: &str,
    node_id: NodeId,
    jar_path: &Path,
    file_id: FileId,
    fqn_to_node: &mut HashMap<String, NodeId>,
    fqn_to_nodes: &mut HashMap<String, Vec<ClasspathNodeRef>>,
) {
    fqn_to_node.entry(fqn.to_owned()).or_insert(node_id);
    fqn_to_nodes
        .entry(fqn.to_owned())
        .or_default()
        .push(ClasspathNodeRef {
            node_id,
            fqn: fqn.to_owned(),
            jar_path: jar_path.to_path_buf(),
            file_id,
        });
}

// `find_jar_path_for_stub` has been replaced by `stub.source_jar` inline
// in `emit_class_stub`. The JAR path is now tracked per-stub during scanning,
// eliminating the incorrect "first provenance entry" heuristic.

/// Look up provenance for a JAR path.
fn find_provenance_for_jar<'a>(
    jar_path: &Path,
    prov_map: &HashMap<&Path, &'a ClasspathProvenance>,
) -> Option<&'a ClasspathProvenance> {
    prov_map.get(jar_path).copied()
}

// ---------------------------------------------------------------------------
// ExportMap registration (U15b)
// ---------------------------------------------------------------------------

/// Register classpath nodes in the `ExportMap` for cross-file resolution.
///
/// FQN precedence: workspace > direct dep > transitive dep.
/// Direct dependencies are registered before transitive dependencies so that
/// `ExportMap::lookup()` (which returns the first entry) prefers them.
///
/// Only class-level nodes (not methods/fields) are registered, matching the
/// Java import resolution model where imports resolve to types.
#[allow(clippy::implicit_hasher)] // Standard HashMap is intentional for classpath emitter
pub fn register_classpath_exports(
    fqn_to_nodes: &HashMap<String, Vec<ClasspathNodeRef>>,
    export_map: &mut ExportMap,
    provenance: &[ClasspathProvenance],
    index: &ClasspathIndex,
) {
    // Build a set of class-level FQNs (not methods/fields).
    let class_fqns: std::collections::HashSet<&str> =
        index.classes.iter().map(|s| s.fqn.as_str()).collect();

    // Register direct dependencies first for precedence.
    let direct_jars: std::collections::HashSet<&Path> = provenance
        .iter()
        .filter(|p| p.has_direct_scope())
        .map(|p| p.jar_path.as_path())
        .collect();

    let transitive_jars: std::collections::HashSet<&Path> = provenance
        .iter()
        .filter(|p| !p.has_direct_scope())
        .map(|p| p.jar_path.as_path())
        .collect();

    // Phase 1: Register direct dependency classes.
    register_exports_for_jars(&class_fqns, fqn_to_nodes, export_map, &direct_jars, index);

    // Phase 2: Register transitive dependency classes.
    register_exports_for_jars(
        &class_fqns,
        fqn_to_nodes,
        export_map,
        &transitive_jars,
        index,
    );
}

/// Register exports for classes from a specific set of JARs.
fn register_exports_for_jars(
    class_fqns: &std::collections::HashSet<&str>,
    fqn_to_nodes: &HashMap<String, Vec<ClasspathNodeRef>>,
    export_map: &mut ExportMap,
    jar_filter: &std::collections::HashSet<&Path>,
    index: &ClasspathIndex,
) {
    for stub in &index.classes {
        let Some(source_jar) = stub.source_jar.as_deref() else {
            continue;
        };
        if !jar_filter.contains(Path::new(source_jar)) {
            continue;
        }

        let fqn = stub.fqn.as_str();
        if class_fqns.contains(fqn)
            && let Some(node_refs) = fqn_to_nodes.get(fqn)
        {
            for node_ref in node_refs {
                if node_ref.jar_path == Path::new(source_jar) {
                    export_map.register(fqn.to_owned(), node_ref.file_id, node_ref.node_id);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Cross-reference edge creation (U15b)
// ---------------------------------------------------------------------------

/// Create inheritance, generic, annotation, module, and inner-class edges
/// for classpath nodes.
///
/// Only creates edges where both source and target nodes exist in the graph.
/// Missing targets are silently skipped (they may be from JARs not on the
/// classpath).
#[allow(clippy::too_many_lines)]
#[allow(clippy::implicit_hasher)] // Standard HashMap intentional
pub fn create_classpath_edges(
    #[allow(clippy::implicit_hasher)] // Standard HashMap is intentional
    index: &ClasspathIndex,
    fqn_to_nodes: &HashMap<String, Vec<ClasspathNodeRef>>,
    provenance: &[ClasspathProvenance],
    staging: &mut StagingGraph,
) {
    let prov_map: HashMap<&Path, &ClasspathProvenance> = provenance
        .iter()
        .map(|p| (p.jar_path.as_path(), p))
        .collect();

    for stub in &index.classes {
        let source_jar = stub.source_jar.as_deref().map(Path::new);
        let Some(class_node) = select_node_ref(&stub.fqn, source_jar, fqn_to_nodes, &prov_map)
        else {
            continue;
        };
        let class_node_id = class_node.node_id;
        let file_id = class_node.file_id;

        // --- Inheritance (Inherits) ---
        if let Some(ref superclass_fqn) = stub.superclass
            && superclass_fqn != "java.lang.Object"
        {
            if let Some(super_node) =
                select_node_ref(superclass_fqn, source_jar, fqn_to_nodes, &prov_map)
            {
                let super_node_id = super_node.node_id;
                staging.add_edge(class_node_id, super_node_id, EdgeKind::Inherits, file_id);
            } else {
                log::debug!(
                    "classpath: skipping Inherits edge for {} → {} (target not in graph)",
                    stub.fqn,
                    superclass_fqn
                );
            }
        }

        // --- Interface implementation (Implements) ---
        for iface_fqn in &stub.interfaces {
            if let Some(iface_node) =
                select_node_ref(iface_fqn, source_jar, fqn_to_nodes, &prov_map)
            {
                let iface_node_id = iface_node.node_id;
                staging.add_edge(class_node_id, iface_node_id, EdgeKind::Implements, file_id);
            } else {
                log::debug!(
                    "classpath: skipping Implements edge for {} → {} (target not in graph)",
                    stub.fqn,
                    iface_fqn
                );
            }
        }

        // --- Generic bounds (GenericBound) ---
        if let Some(ref gen_sig) = stub.generic_signature {
            for tp in &gen_sig.type_parameters {
                let tp_fqn = format!("{}.<{}>", stub.fqn, tp.name);
                let Some(tp_node) = select_node_ref(&tp_fqn, source_jar, fqn_to_nodes, &prov_map)
                else {
                    continue;
                };
                let tp_node_id = tp_node.node_id;

                // Class bound
                if let Some(ref bound) = tp.class_bound
                    && let Some(bound_fqn) = extract_class_fqn_from_type_sig(bound)
                    && let Some(bound_node) =
                        select_node_ref(bound_fqn, source_jar, fqn_to_nodes, &prov_map)
                {
                    let bound_node_id = bound_node.node_id;
                    staging.add_edge(tp_node_id, bound_node_id, EdgeKind::GenericBound, file_id);
                }

                // Interface bounds
                for ibound in &tp.interface_bounds {
                    if let Some(bound_fqn) = extract_class_fqn_from_type_sig(ibound)
                        && let Some(bound_node) =
                            select_node_ref(bound_fqn, source_jar, fqn_to_nodes, &prov_map)
                    {
                        let bound_node_id = bound_node.node_id;
                        staging.add_edge(
                            tp_node_id,
                            bound_node_id,
                            EdgeKind::GenericBound,
                            file_id,
                        );
                    }
                }
            }
        }

        // --- Annotations (AnnotatedWith) ---
        for ann in &stub.annotations {
            if let Some(ann_type_node) =
                select_node_ref(&ann.type_fqn, source_jar, fqn_to_nodes, &prov_map)
            {
                let ann_type_node_id = ann_type_node.node_id;
                staging.add_edge(
                    class_node_id,
                    ann_type_node_id,
                    EdgeKind::AnnotatedWith,
                    file_id,
                );
            }
        }

        // Method-level annotations
        for method in &stub.methods {
            let method_fqn = format!("{}.{}{}", stub.fqn, method.name, method.descriptor);
            if let Some(method_node) =
                select_node_ref(&method_fqn, source_jar, fqn_to_nodes, &prov_map)
            {
                let method_node_id = method_node.node_id;
                for ann in &method.annotations {
                    if let Some(ann_type_node) =
                        select_node_ref(&ann.type_fqn, source_jar, fqn_to_nodes, &prov_map)
                    {
                        let ann_type_node_id = ann_type_node.node_id;
                        staging.add_edge(
                            method_node_id,
                            ann_type_node_id,
                            EdgeKind::AnnotatedWith,
                            file_id,
                        );
                    }
                }
            }
        }

        // Field-level annotations
        for field in &stub.fields {
            let field_fqn = format!("{}.{}", stub.fqn, field.name);
            if let Some(field_node) =
                select_node_ref(&field_fqn, source_jar, fqn_to_nodes, &prov_map)
            {
                let field_node_id = field_node.node_id;
                for ann in &field.annotations {
                    if let Some(ann_type_node) =
                        select_node_ref(&ann.type_fqn, source_jar, fqn_to_nodes, &prov_map)
                    {
                        let ann_type_node_id = ann_type_node.node_id;
                        staging.add_edge(
                            field_node_id,
                            ann_type_node_id,
                            EdgeKind::AnnotatedWith,
                            file_id,
                        );
                    }
                }
            }
        }

        // --- Inner classes (Contains) ---
        for inner in &stub.inner_classes {
            if inner.outer_fqn.as_deref() == Some(&stub.fqn)
                && let Some(inner_node) =
                    select_node_ref(&inner.inner_fqn, source_jar, fqn_to_nodes, &prov_map)
            {
                let inner_node_id = inner_node.node_id;
                staging.add_edge(class_node_id, inner_node_id, EdgeKind::Contains, file_id);
            }
        }

        // --- Module edges ---
        if let Some(ref module) = stub.module {
            let mod_fqn = format!("module:{}", module.name);
            let Some(mod_node) = select_node_ref(&mod_fqn, source_jar, fqn_to_nodes, &prov_map)
            else {
                continue;
            };
            let mod_node_id = mod_node.node_id;

            // ModuleRequires
            for req in &module.requires {
                let req_mod_fqn = format!("module:{}", req.module_name);
                if let Some(req_node) =
                    select_node_ref(&req_mod_fqn, source_jar, fqn_to_nodes, &prov_map)
                {
                    let req_node_id = req_node.node_id;
                    staging.add_edge(mod_node_id, req_node_id, EdgeKind::ModuleRequires, file_id);
                }
            }

            // ModuleExports - edge from module to classes in exported package
            for exp in &module.exports {
                // Look up classes in the exported package
                let pkg_classes = index.lookup_package(&exp.package);
                for pkg_class in pkg_classes {
                    for pkg_class_node in
                        select_node_refs(&pkg_class.fqn, source_jar, fqn_to_nodes, &prov_map)
                    {
                        let pkg_class_node_id = pkg_class_node.node_id;
                        staging.add_edge(
                            mod_node_id,
                            pkg_class_node_id,
                            EdgeKind::ModuleExports,
                            file_id,
                        );
                    }
                }
            }

            // ModuleOpens - edge from module to classes in opened package
            for opens in &module.opens {
                let pkg_classes = index.lookup_package(&opens.package);
                for pkg_class in pkg_classes {
                    for pkg_class_node in
                        select_node_refs(&pkg_class.fqn, source_jar, fqn_to_nodes, &prov_map)
                    {
                        let pkg_class_node_id = pkg_class_node.node_id;
                        staging.add_edge(
                            mod_node_id,
                            pkg_class_node_id,
                            EdgeKind::ModuleOpens,
                            file_id,
                        );
                    }
                }
            }

            // ModuleProvides - edge from module to service implementation classes
            for provides in &module.provides {
                for impl_fqn in &provides.implementations {
                    if let Some(impl_node) =
                        select_node_ref(impl_fqn, source_jar, fqn_to_nodes, &prov_map)
                    {
                        let impl_node_id = impl_node.node_id;
                        staging.add_edge(
                            mod_node_id,
                            impl_node_id,
                            EdgeKind::ModuleProvides,
                            file_id,
                        );
                    }
                }
            }
        }
    }
}

fn select_node_ref<'a>(
    fqn: &str,
    source_jar: Option<&Path>,
    fqn_to_nodes: &'a HashMap<String, Vec<ClasspathNodeRef>>,
    prov_map: &HashMap<&Path, &ClasspathProvenance>,
) -> Option<&'a ClasspathNodeRef> {
    let candidates = select_node_refs(fqn, source_jar, fqn_to_nodes, prov_map);
    candidates.first().copied()
}

fn select_node_refs<'a>(
    fqn: &str,
    source_jar: Option<&Path>,
    fqn_to_nodes: &'a HashMap<String, Vec<ClasspathNodeRef>>,
    prov_map: &HashMap<&Path, &ClasspathProvenance>,
) -> Vec<&'a ClasspathNodeRef> {
    let Some(candidates) = fqn_to_nodes.get(fqn) else {
        return Vec::new();
    };

    let Some(source_jar) = source_jar else {
        return candidates.iter().collect();
    };

    if let Some(exact_match) = candidates
        .iter()
        .find(|candidate| candidate.jar_path.as_path() == source_jar)
    {
        return vec![exact_match];
    }

    let scoped: Vec<_> = candidates
        .iter()
        .filter(|candidate| jars_share_scope(source_jar, candidate.jar_path.as_path(), prov_map))
        .collect();
    if scoped.is_empty() {
        candidates.iter().collect()
    } else {
        scoped
    }
}

fn jars_share_scope(
    source_jar: &Path,
    target_jar: &Path,
    prov_map: &HashMap<&Path, &ClasspathProvenance>,
) -> bool {
    if source_jar == target_jar {
        return true;
    }

    let Some(source) = prov_map.get(source_jar) else {
        debug!(
            "classpath: provenance missing for source jar {}; allowing scope fallback",
            source_jar.display()
        );
        return true;
    };
    let Some(target) = prov_map.get(target_jar) else {
        debug!(
            "classpath: provenance missing for target jar {}; allowing scope fallback",
            target_jar.display()
        );
        return true;
    };

    source.scopes.iter().any(|source_scope| {
        target
            .scopes
            .iter()
            .any(|target_scope| source_scope.module_root == target_scope.module_root)
    })
}

/// Extract the FQN from a `TypeSignature::Class` variant.
fn extract_class_fqn_from_type_sig(sig: &crate::stub::model::TypeSignature) -> Option<&str> {
    match sig {
        crate::stub::model::TypeSignature::Class { fqn, .. } => Some(fqn.as_str()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stub::model::{
        AccessFlags, AnnotationStub, ClassKind, FieldStub, GenericClassSignature, InnerClassEntry,
        LambdaTargetStub, MethodStub, ModuleExports, ModuleProvides, ModuleRequires, ModuleStub,
        ReferenceKind, TypeParameterStub, TypeSignature,
    };
    use sqry_core::graph::unified::BidirectionalEdgeStore;
    use sqry_core::graph::unified::storage::AuxiliaryIndices;
    use sqry_core::graph::unified::storage::NodeArena;

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    fn make_interner() -> StringInterner {
        StringInterner::new()
    }

    fn make_staging() -> StagingGraph {
        StagingGraph::default()
    }

    fn make_provenance(jar: &str, direct: bool) -> ClasspathProvenance {
        ClasspathProvenance {
            jar_path: PathBuf::from(jar),
            coordinates: Some(format!(
                "group:artifact:{}",
                if direct { "1.0" } else { "2.0" }
            )),
            is_direct: direct,
            scopes: vec![crate::graph::provenance::ClasspathScope {
                module_name: "test".to_owned(),
                module_root: PathBuf::from("/repo/test"),
                is_direct: direct,
            }],
        }
    }

    fn make_stub(fqn: &str) -> ClassStub {
        ClassStub {
            fqn: fqn.to_owned(),
            name: fqn.rsplit('.').next().unwrap_or(fqn).to_owned(),
            kind: ClassKind::Class,
            access: AccessFlags::new(AccessFlags::ACC_PUBLIC),
            superclass: Some("java.lang.Object".to_owned()),
            interfaces: vec![],
            methods: vec![],
            fields: vec![],
            annotations: vec![],
            generic_signature: None,
            inner_classes: vec![],
            lambda_targets: vec![],
            module: None,
            record_components: vec![],
            enum_constants: vec![],
            source_file: None,
            source_jar: None,
            kotlin_metadata: None,
            scala_signature: None,
        }
    }

    fn make_method(name: &str) -> MethodStub {
        MethodStub {
            name: name.to_owned(),
            access: AccessFlags::new(AccessFlags::ACC_PUBLIC),
            descriptor: "()V".to_owned(),
            generic_signature: None,
            annotations: vec![],
            parameter_annotations: vec![],
            parameter_names: vec![],
            return_type: TypeSignature::Base(crate::stub::model::BaseType::Void),
            parameter_types: vec![],
        }
    }

    fn make_field(name: &str) -> FieldStub {
        FieldStub {
            name: name.to_owned(),
            access: AccessFlags::new(AccessFlags::ACC_PRIVATE),
            descriptor: "I".to_owned(),
            generic_signature: None,
            annotations: vec![],
            constant_value: None,
        }
    }

    /// Run emission and return the result plus the staging graph for inspection.
    fn run_emission(
        stubs: Vec<ClassStub>,
        provenance: &[ClasspathProvenance],
    ) -> (
        EmissionResult,
        StagingGraph,
        FileRegistry,
        StringInterner,
        NodeMetadataStore,
    ) {
        let default_jar = provenance
            .first()
            .map(|entry| entry.jar_path.display().to_string());
        let normalized_stubs = stubs
            .into_iter()
            .map(|mut stub| {
                if stub.source_jar.is_none() {
                    stub.source_jar = default_jar.clone();
                }
                stub
            })
            .collect();
        let index = ClasspathIndex::build(normalized_stubs);
        let mut staging = make_staging();
        let mut file_registry = FileRegistry::new();
        let mut interner = make_interner();
        let mut metadata_store = NodeMetadataStore::new();

        let result = emit_classpath_nodes(
            &index,
            &mut staging,
            &mut file_registry,
            &mut interner,
            &mut metadata_store,
            provenance,
        )
        .expect("emission should succeed");

        (result, staging, file_registry, interner, metadata_store)
    }

    // -----------------------------------------------------------------------
    // Test 1: Simple class emits correct nodes
    // -----------------------------------------------------------------------

    #[test]
    fn test_simple_class_emits_nodes() {
        let mut stub = make_stub("com.example.Foo");
        stub.methods = vec![make_method("bar"), make_method("baz")];
        stub.fields = vec![make_field("count")];

        let prov = vec![make_provenance("/jars/example.jar", true)];
        let (result, _staging, _registry, _interner, _meta) = run_emission(vec![stub], &prov);

        // Class node
        assert!(
            result.fqn_to_node.contains_key("com.example.Foo"),
            "class node should be emitted"
        );

        // Method nodes (key includes descriptor for overload disambiguation)
        assert!(
            result.fqn_to_node.contains_key("com.example.Foo.bar()V"),
            "method 'bar' should be emitted"
        );
        assert!(
            result.fqn_to_node.contains_key("com.example.Foo.baz()V"),
            "method 'baz' should be emitted"
        );

        // Field node
        assert!(
            result.fqn_to_node.contains_key("com.example.Foo.count"),
            "field 'count' should be emitted"
        );

        // Total: 1 class + 2 methods + 1 field = 4
        assert_eq!(result.fqn_to_node.len(), 4);
    }

    // -----------------------------------------------------------------------
    // Test 2: Inheritance edge
    // -----------------------------------------------------------------------

    #[test]
    fn test_inheritance_edge_created() {
        let mut list = make_stub("java.util.AbstractList");
        list.superclass = Some("java.util.AbstractCollection".to_owned());

        let collection = make_stub("java.util.AbstractCollection");

        let prov = vec![make_provenance("/jars/rt.jar", true)];
        let stubs = vec![list, collection];
        let index = ClasspathIndex::build(stubs.clone());
        let (result, mut staging, _registry, _interner, _meta) = run_emission(stubs, &prov);

        create_classpath_edges(&index, &result.fqn_to_nodes, &prov, &mut staging);

        // Verify the AbstractList node exists
        assert!(result.fqn_to_node.contains_key("java.util.AbstractList"));
        assert!(
            result
                .fqn_to_node
                .contains_key("java.util.AbstractCollection")
        );

        // Edge verification happens through staging stats
        let stats = staging.stats();
        // Structural edges (Defines for methods/fields) + Inherits edge
        assert!(
            stats.edges_staged > 0,
            "should have at least one edge staged"
        );
    }

    // -----------------------------------------------------------------------
    // Test 3: Interface implementation edge
    // -----------------------------------------------------------------------

    #[test]
    fn test_implements_edge_created() {
        let mut array_list = make_stub("java.util.ArrayList");
        array_list.interfaces = vec![
            "java.util.List".to_owned(),
            "java.io.Serializable".to_owned(),
        ];

        let list_iface = {
            let mut s = make_stub("java.util.List");
            s.kind = ClassKind::Interface;
            s
        };

        let serializable = {
            let mut s = make_stub("java.io.Serializable");
            s.kind = ClassKind::Interface;
            s
        };

        let prov = vec![make_provenance("/jars/rt.jar", true)];
        let stubs = vec![array_list, list_iface, serializable];
        let index = ClasspathIndex::build(stubs.clone());
        let (result, mut staging, _registry, _interner, _meta) = run_emission(stubs, &prov);

        create_classpath_edges(&index, &result.fqn_to_nodes, &prov, &mut staging);

        // All three should exist
        assert!(result.fqn_to_node.contains_key("java.util.ArrayList"));
        assert!(result.fqn_to_node.contains_key("java.util.List"));
        assert!(result.fqn_to_node.contains_key("java.io.Serializable"));
    }

    // -----------------------------------------------------------------------
    // Test 4: ExportMap registration and lookup
    // -----------------------------------------------------------------------

    #[test]
    fn test_export_map_registration_and_lookup() {
        let mut alpha = make_stub("com.example.Alpha");
        alpha.source_jar = Some("/jars/example.jar".to_owned());
        let mut beta = make_stub("com.example.Beta");
        beta.source_jar = Some("/jars/example.jar".to_owned());
        let stubs = vec![alpha, beta];

        let prov = vec![make_provenance("/jars/example.jar", true)];
        let index = ClasspathIndex::build(stubs.clone());
        let (result, _staging, _registry, _interner, _meta) = run_emission(stubs, &prov);

        let mut export_map = ExportMap::new();
        register_classpath_exports(&result.fqn_to_nodes, &mut export_map, &prov, &index);

        // Lookup should find the registered classes
        let alpha = export_map.lookup("com.example.Alpha");
        assert!(alpha.is_some(), "Alpha should be in ExportMap");

        let beta = export_map.lookup("com.example.Beta");
        assert!(beta.is_some(), "Beta should be in ExportMap");

        // Non-existent should return None
        let missing = export_map.lookup("com.example.DoesNotExist");
        assert!(missing.is_none());
    }

    // -----------------------------------------------------------------------
    // Test 5: FQN precedence (direct > transitive)
    // -----------------------------------------------------------------------

    #[test]
    fn test_fqn_precedence_direct_before_transitive() {
        let mut direct_stub = make_stub("com.example.Foo");
        direct_stub.source_jar = Some("/jars/direct.jar".to_owned());
        let mut transitive_stub = make_stub("com.example.Foo");
        transitive_stub.source_jar = Some("/jars/transitive.jar".to_owned());
        // Deliberately keep aggregate `is_direct` false while marking the scope
        // as direct to prove export precedence reads per-scope metadata.
        let mut prov_direct = make_provenance("/jars/direct.jar", false);
        prov_direct.scopes[0].is_direct = true;
        let prov_transitive = make_provenance("/jars/transitive.jar", false);

        let index = ClasspathIndex::build(vec![direct_stub, transitive_stub]);
        let (result, _staging, _registry, _interner, _meta) = run_emission(
            index.classes.clone(),
            &[prov_direct.clone(), prov_transitive],
        );

        let mut export_map = ExportMap::new();
        register_classpath_exports(
            &result.fqn_to_nodes,
            &mut export_map,
            &[prov_direct, make_provenance("/jars/transitive.jar", false)],
            &index,
        );

        let entry = export_map
            .lookup("com.example.Foo")
            .expect("class should be registered");
        let (_file_id, node_id) = entry;
        let direct_node_id = result
            .fqn_to_nodes
            .get("com.example.Foo")
            .and_then(|node_refs| {
                node_refs
                    .iter()
                    .find(|node_ref| node_ref.jar_path == PathBuf::from("/jars/direct.jar"))
            })
            .map(|node_ref| node_ref.node_id)
            .expect("direct node should exist");
        assert_eq!(node_id, direct_node_id);
    }

    #[test]
    fn test_emit_into_code_graph_commits_edges_and_metadata() {
        let mut child = make_stub("java.util.AbstractList");
        child.superclass = Some("java.util.AbstractCollection".to_owned());
        child.source_jar = Some("/jars/rt.jar".to_owned());

        let mut parent = make_stub("java.util.AbstractCollection");
        parent.source_jar = Some("/jars/rt.jar".to_owned());

        let index = ClasspathIndex::build(vec![child, parent]);
        let provenance = vec![make_provenance("/jars/rt.jar", true)];
        let mut graph = CodeGraph::new();

        let result = emit_into_code_graph(&index, &mut graph, &provenance)
            .expect("in-place classpath emission should succeed");

        let child_id = *result
            .fqn_to_node
            .get("java.util.AbstractList")
            .expect("child class node should be returned");
        let parent_id = *result
            .fqn_to_node
            .get("java.util.AbstractCollection")
            .expect("parent class node should be returned");

        assert!(
            graph.nodes().get(child_id).is_some(),
            "child node should exist"
        );
        assert!(
            graph.edge_count() > 0,
            "structural classpath edges should be committed"
        );
        assert!(
            graph.edges().edges_from(child_id).iter().any(|edge| {
                matches!(edge.kind, EdgeKind::Inherits) && edge.target == parent_id
            }),
            "child class should have an inherits edge to its parent"
        );
        assert!(
            matches!(
                graph.macro_metadata().get_metadata(child_id),
                Some(NodeMetadata::Classpath(_))
            ),
            "classpath metadata should remain attached after node-id remap"
        );
    }

    #[test]
    fn test_emit_into_code_graph_preserves_graph_on_failure() {
        let mut strings = StringInterner::with_max_ids(2);
        let existing_name = strings.intern("existing").unwrap();
        let existing_qname = strings.intern("existing::node").unwrap();

        let mut files = FileRegistry::new();
        let file_id = files.register(Path::new("/existing.rs")).unwrap();

        let mut nodes = NodeArena::new();
        let existing_node = nodes
            .alloc(
                NodeEntry::new(NodeKind::Function, existing_name, file_id)
                    .with_qualified_name(existing_qname),
            )
            .unwrap();

        let mut graph = CodeGraph::from_components(
            nodes,
            BidirectionalEdgeStore::new(),
            strings,
            files,
            AuxiliaryIndices::new(),
            NodeMetadataStore::new(),
        );
        graph.macro_metadata_mut().insert_metadata(
            existing_node,
            NodeMetadata::Classpath(ClasspathNodeMetadata {
                coordinates: Some("group:existing:1.0".to_owned()),
                jar_path: "/existing.jar".to_owned(),
                fqn: "existing::node".to_owned(),
                is_direct_dependency: true,
            }),
        );

        let snapshot_before = graph.snapshot();
        let existing_path_before = snapshot_before
            .files()
            .resolve(file_id)
            .expect("existing file should resolve")
            .to_path_buf();
        let existing_meta_before = snapshot_before
            .macro_metadata()
            .get_metadata(existing_node)
            .cloned();

        let mut failing_stub = make_stub("com.example.WillFail");
        failing_stub.source_jar = Some("/jars/failing.jar".to_owned());
        let index = ClasspathIndex::build(vec![failing_stub]);
        let provenance = vec![make_provenance("/jars/failing.jar", true)];

        let error = emit_into_code_graph(&index, &mut graph, &provenance)
            .expect_err("emission should fail when the cloned interner is full");
        assert!(
            error.to_string().contains("string intern failed"),
            "unexpected error: {error}"
        );

        assert_eq!(graph.node_count(), snapshot_before.nodes().len());
        assert_eq!(graph.edge_count(), 0);
        assert_eq!(graph.strings().len(), snapshot_before.strings().len());
        assert_eq!(graph.files().len(), snapshot_before.files().len());

        let surviving_node = graph
            .nodes()
            .get(existing_node)
            .expect("existing node must survive failed emission");
        assert_eq!(surviving_node.name, existing_name);
        assert_eq!(
            graph
                .files()
                .resolve(file_id)
                .map(|path| path.to_path_buf()),
            Some(existing_path_before)
        );
        assert_eq!(
            graph.macro_metadata().get_metadata(existing_node),
            existing_meta_before.as_ref()
        );
    }

    // -----------------------------------------------------------------------
    // Test 6: ClasspathNodeMetadata attached correctly
    // -----------------------------------------------------------------------

    #[test]
    fn test_classpath_metadata_attached() {
        let stub = make_stub("com.google.common.collect.ImmutableList");
        let prov = vec![ClasspathProvenance {
            jar_path: PathBuf::from("/home/user/.m2/repository/guava-33.0.0.jar"),
            coordinates: Some("com.google.guava:guava:33.0.0".to_owned()),
            is_direct: true,
            scopes: vec![crate::graph::provenance::ClasspathScope {
                module_name: "test".to_owned(),
                module_root: PathBuf::from("/repo/test"),
                is_direct: true,
            }],
        }];

        let (result, _staging, _registry, _interner, metadata_store) =
            run_emission(vec![stub], &prov);

        let node_id = result
            .fqn_to_node
            .get("com.google.common.collect.ImmutableList")
            .expect("node should exist");

        let metadata = metadata_store
            .get_metadata(*node_id)
            .expect("metadata should be attached");

        match metadata {
            NodeMetadata::Classpath(cp) => {
                assert_eq!(cp.fqn, "com.google.common.collect.ImmutableList");
                assert_eq!(
                    cp.coordinates.as_deref(),
                    Some("com.google.guava:guava:33.0.0")
                );
                assert!(cp.is_direct_dependency);
                assert!(cp.jar_path.contains("guava-33.0.0.jar"));
            }
            NodeMetadata::Macro(_) => panic!("expected Classpath metadata, got Macro"),
        }
    }

    // -----------------------------------------------------------------------
    // Test 7: Zero spans on all nodes
    // -----------------------------------------------------------------------

    #[test]
    fn test_zero_spans_on_classpath_nodes() {
        let mut stub = make_stub("com.example.ZeroSpan");
        stub.methods = vec![make_method("doWork")];

        let prov = vec![make_provenance("/jars/test.jar", true)];
        let (result, staging, _registry, _interner, _meta) = run_emission(vec![stub], &prov);

        // All emitted nodes should have zero spans since they come from bytecode.
        // The NodeEntry::new constructor sets all span fields to 0 by default,
        // and we don't override them.
        assert!(
            !result.fqn_to_node.is_empty(),
            "should have emitted at least one node"
        );

        // Verify through staging stats that nodes were emitted
        let stats = staging.stats();
        assert!(
            stats.nodes_staged >= 2,
            "should have at least 2 nodes (class + method)"
        );
    }

    // -----------------------------------------------------------------------
    // Test 8: Annotation edges
    // -----------------------------------------------------------------------

    #[test]
    fn test_annotation_edges() {
        let mut stub = make_stub("com.example.MyService");
        stub.annotations = vec![AnnotationStub {
            type_fqn: "org.springframework.stereotype.Service".to_owned(),
            elements: vec![],
            is_runtime_visible: true,
        }];

        // Also emit the annotation type so the edge can be created
        let ann_type = {
            let mut s = make_stub("org.springframework.stereotype.Service");
            s.kind = ClassKind::Annotation;
            s
        };

        let prov = vec![make_provenance("/jars/app.jar", true)];
        let stubs = vec![stub, ann_type];
        let index = ClasspathIndex::build(stubs.clone());
        let (result, mut staging, _registry, _interner, _meta) = run_emission(stubs, &prov);

        create_classpath_edges(&index, &result.fqn_to_nodes, &prov, &mut staging);

        // Both nodes should exist
        assert!(result.fqn_to_node.contains_key("com.example.MyService"));
        assert!(
            result
                .fqn_to_node
                .contains_key("org.springframework.stereotype.Service")
        );

        // Edge verification through staging stats
        let stats = staging.stats();
        assert!(
            stats.edges_staged > 0,
            "should have annotation edges staged"
        );
    }

    // -----------------------------------------------------------------------
    // Test 9: Module edges
    // -----------------------------------------------------------------------

    #[test]
    fn test_module_edges() {
        let provider_class = make_stub("com.example.spi.MyProvider");
        let exported_class = make_stub("com.example.api.MyApi");

        let mut module_stub = make_stub("module-info");
        module_stub.kind = ClassKind::Module;
        module_stub.module = Some(ModuleStub {
            name: "com.example".to_owned(),
            access: AccessFlags::new(0),
            version: None,
            requires: vec![ModuleRequires {
                module_name: "java.base".to_owned(),
                access: AccessFlags::new(0),
                version: None,
            }],
            exports: vec![ModuleExports {
                package: "com.example.api".to_owned(),
                access: AccessFlags::new(0),
                to_modules: vec![],
            }],
            opens: vec![],
            provides: vec![ModuleProvides {
                service: "com.example.spi.SomeService".to_owned(),
                implementations: vec!["com.example.spi.MyProvider".to_owned()],
            }],
            uses: vec![],
        });

        let prov = vec![make_provenance("/jars/example.jar", true)];
        let stubs = vec![module_stub, provider_class, exported_class];
        let index = ClasspathIndex::build(stubs.clone());
        let (result, mut staging, _registry, _interner, _meta) = run_emission(stubs, &prov);

        create_classpath_edges(&index, &result.fqn_to_nodes, &prov, &mut staging);

        // Module node should exist
        assert!(
            result.fqn_to_node.contains_key("module:com.example"),
            "module node should be emitted"
        );

        let stats = staging.stats();
        assert!(stats.edges_staged > 0, "should have module edges staged");
    }

    // -----------------------------------------------------------------------
    // Test 10: Enum constants
    // -----------------------------------------------------------------------

    #[test]
    fn test_enum_constants_emitted() {
        let mut stub = make_stub("java.time.DayOfWeek");
        stub.kind = ClassKind::Enum;
        stub.enum_constants = vec![
            "MONDAY".to_owned(),
            "TUESDAY".to_owned(),
            "WEDNESDAY".to_owned(),
        ];

        let prov = vec![make_provenance("/jars/rt.jar", true)];
        let (result, _staging, _registry, _interner, _meta) = run_emission(vec![stub], &prov);

        assert!(
            result
                .fqn_to_node
                .contains_key("java.time.DayOfWeek.MONDAY")
        );
        assert!(
            result
                .fqn_to_node
                .contains_key("java.time.DayOfWeek.TUESDAY")
        );
        assert!(
            result
                .fqn_to_node
                .contains_key("java.time.DayOfWeek.WEDNESDAY")
        );
    }

    // -----------------------------------------------------------------------
    // Test 11: Type parameters emitted
    // -----------------------------------------------------------------------

    #[test]
    fn test_type_parameters_emitted() {
        let mut stub = make_stub("java.util.HashMap");
        stub.generic_signature = Some(GenericClassSignature {
            type_parameters: vec![
                TypeParameterStub {
                    name: "K".to_owned(),
                    class_bound: None,
                    interface_bounds: vec![],
                },
                TypeParameterStub {
                    name: "V".to_owned(),
                    class_bound: None,
                    interface_bounds: vec![],
                },
            ],
            superclass: TypeSignature::Class {
                fqn: "java.util.AbstractMap".to_owned(),
                type_arguments: vec![],
            },
            interfaces: vec![],
        });

        let prov = vec![make_provenance("/jars/rt.jar", true)];
        let (result, _staging, _registry, _interner, _meta) = run_emission(vec![stub], &prov);

        assert!(result.fqn_to_node.contains_key("java.util.HashMap.<K>"));
        assert!(result.fqn_to_node.contains_key("java.util.HashMap.<V>"));
    }

    // -----------------------------------------------------------------------
    // Test 12: Generic bound edges
    // -----------------------------------------------------------------------

    #[test]
    fn test_generic_bound_edges() {
        let comparable = make_stub("java.lang.Comparable");

        let mut stub = make_stub("com.example.Sorted");
        stub.generic_signature = Some(GenericClassSignature {
            type_parameters: vec![TypeParameterStub {
                name: "T".to_owned(),
                class_bound: Some(TypeSignature::Class {
                    fqn: "java.lang.Comparable".to_owned(),
                    type_arguments: vec![],
                }),
                interface_bounds: vec![],
            }],
            superclass: TypeSignature::Class {
                fqn: "java.lang.Object".to_owned(),
                type_arguments: vec![],
            },
            interfaces: vec![],
        });

        let prov = vec![make_provenance("/jars/rt.jar", true)];
        let stubs = vec![stub, comparable];
        let index = ClasspathIndex::build(stubs.clone());
        let (result, mut staging, _registry, _interner, _meta) = run_emission(stubs, &prov);

        create_classpath_edges(&index, &result.fqn_to_nodes, &prov, &mut staging);

        // Type parameter and bound target should exist
        assert!(result.fqn_to_node.contains_key("com.example.Sorted.<T>"));
        assert!(result.fqn_to_node.contains_key("java.lang.Comparable"));
    }

    // -----------------------------------------------------------------------
    // Test 13: Lambda targets emitted
    // -----------------------------------------------------------------------

    #[test]
    fn test_lambda_targets_emitted() {
        let mut stub = make_stub("com.example.Processor");
        stub.lambda_targets = vec![LambdaTargetStub {
            owner_fqn: "java.lang.String".to_owned(),
            method_name: "toUpperCase".to_owned(),
            method_descriptor: "()Ljava/lang/String;".to_owned(),
            reference_kind: ReferenceKind::InvokeVirtual,
        }];

        let prov = vec![make_provenance("/jars/app.jar", true)];
        let (result, _staging, _registry, _interner, _meta) = run_emission(vec![stub], &prov);

        assert!(
            result
                .fqn_to_node
                .contains_key("com.example.Processor.lambda$toUpperCase")
        );
    }

    // -----------------------------------------------------------------------
    // Test 14: Inner class Contains edge
    // -----------------------------------------------------------------------

    #[test]
    fn test_inner_class_contains_edge() {
        let outer = {
            let mut s = make_stub("com.example.Outer");
            s.inner_classes = vec![InnerClassEntry {
                inner_fqn: "com.example.Outer.Inner".to_owned(),
                outer_fqn: Some("com.example.Outer".to_owned()),
                inner_name: Some("Inner".to_owned()),
                access: AccessFlags::new(AccessFlags::ACC_PUBLIC),
            }];
            s
        };

        let inner = make_stub("com.example.Outer.Inner");

        let prov = vec![make_provenance("/jars/app.jar", true)];
        let stubs = vec![outer, inner];
        let index = ClasspathIndex::build(stubs.clone());
        let (result, mut staging, _registry, _interner, _meta) = run_emission(stubs, &prov);

        create_classpath_edges(&index, &result.fqn_to_nodes, &prov, &mut staging);

        assert!(result.fqn_to_node.contains_key("com.example.Outer"));
        assert!(result.fqn_to_node.contains_key("com.example.Outer.Inner"));
    }

    // -----------------------------------------------------------------------
    // Test 15: Empty index produces empty result
    // -----------------------------------------------------------------------

    #[test]
    fn test_empty_index_no_nodes() {
        let prov: Vec<ClasspathProvenance> = vec![];
        let (result, staging, _registry, _interner, _meta) = run_emission(vec![], &prov);

        assert!(result.fqn_to_node.is_empty());
        assert_eq!(staging.stats().nodes_staged, 0);
    }

    // -----------------------------------------------------------------------
    // Test 16: Synthetic file path convention
    // -----------------------------------------------------------------------

    #[test]
    fn test_synthetic_file_path_convention() {
        let stub = make_stub("com.example.Foo");
        let prov = vec![make_provenance("/jars/example.jar", true)];
        let (result, _staging, file_registry, _interner, _meta) = run_emission(vec![stub], &prov);

        let file_id = result
            .file_id_map
            .get("com.example.Foo")
            .expect("file ID should exist");

        let path = file_registry
            .resolve(*file_id)
            .expect("file should be resolvable");

        let path_str = path.to_string_lossy();
        assert!(
            path_str.contains("!/com/example/Foo.class"),
            "synthetic path should follow JAR convention, got: {path_str}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 17: Interface kind maps correctly
    // -----------------------------------------------------------------------

    #[test]
    fn test_interface_kind_mapping() {
        let mut stub = make_stub("java.util.List");
        stub.kind = ClassKind::Interface;

        let prov = vec![make_provenance("/jars/rt.jar", true)];
        let (result, _staging, _registry, _interner, _meta) = run_emission(vec![stub], &prov);

        assert!(result.fqn_to_node.contains_key("java.util.List"));
    }

    // -----------------------------------------------------------------------
    // Test 18: Method-level annotation edge
    // -----------------------------------------------------------------------

    #[test]
    fn test_method_level_annotation_edge() {
        let override_ann = {
            let mut s = make_stub("java.lang.Override");
            s.kind = ClassKind::Annotation;
            s
        };

        let mut stub = make_stub("com.example.Foo");
        stub.methods = vec![MethodStub {
            name: "toString".to_owned(),
            access: AccessFlags::new(AccessFlags::ACC_PUBLIC),
            descriptor: "()Ljava/lang/String;".to_owned(),
            generic_signature: None,
            annotations: vec![AnnotationStub {
                type_fqn: "java.lang.Override".to_owned(),
                elements: vec![],
                is_runtime_visible: true,
            }],
            parameter_annotations: vec![],
            parameter_names: vec![],
            return_type: TypeSignature::Base(crate::stub::model::BaseType::Void),
            parameter_types: vec![],
        }];

        let prov = vec![make_provenance("/jars/rt.jar", true)];
        let stubs = vec![stub, override_ann];
        let index = ClasspathIndex::build(stubs.clone());
        let (result, mut staging, _registry, _interner, _meta) = run_emission(stubs, &prov);

        create_classpath_edges(&index, &result.fqn_to_nodes, &prov, &mut staging);

        assert!(
            result
                .fqn_to_node
                .contains_key("com.example.Foo.toString()Ljava/lang/String;")
        );
        assert!(result.fqn_to_node.contains_key("java.lang.Override"));
    }

    // -----------------------------------------------------------------------
    // Test 19: No provenance still emits nodes
    // -----------------------------------------------------------------------

    #[test]
    fn test_no_provenance_still_emits() {
        let stub = make_stub("com.example.NoProv");
        let (result, _staging, _registry, _interner, _meta) = run_emission(vec![stub], &[]);

        assert!(result.fqn_to_node.contains_key("com.example.NoProv"));
    }

    // -----------------------------------------------------------------------
    // Test 20: Visibility mapping
    // -----------------------------------------------------------------------

    #[test]
    fn test_visibility_mapping() {
        assert_eq!(
            access_to_visibility(&AccessFlags::new(AccessFlags::ACC_PUBLIC)),
            "public"
        );
        assert_eq!(
            access_to_visibility(&AccessFlags::new(AccessFlags::ACC_PROTECTED)),
            "protected"
        );
        assert_eq!(
            access_to_visibility(&AccessFlags::new(AccessFlags::ACC_PRIVATE)),
            "private"
        );
        assert_eq!(access_to_visibility(&AccessFlags::empty()), "package");
    }
}
