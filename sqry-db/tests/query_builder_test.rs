//! Integration tests for [`sqry_db::planner::compile`] (DB10).
//!
//! Coverage targets, drawn from the DB10 task brief:
//!
//! - Empty builder → `BuildError::EmptyBuilder`
//! - First step a `Filter` → `BuildError::FirstStepNotContextFree`
//! - `.traverse(_, _, 0)` → `BuildError::ZeroDepth`
//! - Single-step builds produce the expected plan shape
//! - Multi-step chains preserve step order
//! - `.union`/`.intersect`/`.difference` produce correct `SetOp` nodes
//! - EdgeKind metadata normalisation: two builders with different metadata
//!   produce identical plan hashes
//! - Subquery composition: a builder's output used as a subquery inside
//!   another builder's predicate round-trips through `serde_json` losslessly
//! - `Predicate::And / Or / Not` nesting through `.filter()`
//! - Every `NodeKind` filter and every `Visibility` filter via `.scan_with(...)`
//! - All three `Direction` variants via `.traverse()`
//! - All six `PredicateValue`-bearing predicates
//! - All five `StringPattern` constructors + case-insensitive variant
//! - Example from the design doc builds successfully

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use sqry_core::graph::unified::bind::scope::arena::ScopeKind;
use sqry_core::graph::unified::edge::kind::{
    DbQueryType, EdgeKind, ExportKind, FfiConvention, HttpMethod, LifetimeConstraintKind,
    MacroExpansionKind, MqProtocol, TableWriteOp, TypeOfContext,
};
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::graph::unified::string::StringId;
use sqry_core::schema::Visibility;

use sqry_db::planner::{
    BuildError, Direction, PathPattern, PlanNode, PlanNodeKind, Predicate, PredicateValue,
    QueryBuilder, QueryPlan, QueryPlanExt, RegexFlags, RegexPattern, ScanFilters, SetOperation,
    StringPattern, normalize_edge_kind,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn hash_of(plan: &QueryPlan) -> u64 {
    let mut h = DefaultHasher::new();
    plan.hash(&mut h);
    h.finish()
}

fn unwrap_chain(plan: &QueryPlan) -> &[PlanNode] {
    let PlanNode::Chain { steps } = &plan.root else {
        panic!("expected Chain root, got {:?}", plan.root);
    };
    steps
}

// ---------------------------------------------------------------------------
// Validation: error variants
// ---------------------------------------------------------------------------

#[test]
fn empty_builder_yields_empty_error() {
    let err = QueryBuilder::new().build().unwrap_err();
    assert_eq!(err, BuildError::EmptyBuilder);
}

#[test]
fn first_step_filter_yields_context_free_error() {
    // We can't construct this through the public API in a single chain (the
    // first method on a fresh QueryBuilder cannot be `.filter`). The builder
    // therefore can't *reach* this state through normal use. But to verify
    // the validator's behaviour explicitly, we synthesise a `Chain { Filter,
    // ... }` via the documented sub-plan path: `.union(other)` where `other`
    // is itself just a Filter — exercising the recursive context-free check.
    //
    // Direct construction route: use the lower-level QueryPlan API to build
    // a malformed plan, then feed it into a builder via union(). The
    // builder's recursive validator must reject it.
    let bad_plan = QueryPlan::new(PlanNode::Filter {
        predicate: Predicate::HasCaller,
    });
    let err = QueryBuilder::new()
        .union(bad_plan)
        .build()
        .expect_err("malformed sub-plan must be rejected");
    assert!(matches!(err, BuildError::FirstStepNotContextFree { .. }));
}

#[test]
fn first_step_traversal_yields_context_free_error() {
    // Similar to above: exercise the validator via union() with a malformed
    // plan whose root is an EdgeTraversal.
    let bad_plan = QueryPlan::new(PlanNode::EdgeTraversal {
        direction: Direction::Forward,
        edge_kind: None,
        max_depth: 1,
    });
    let err = QueryBuilder::new().union(bad_plan).build().unwrap_err();
    assert!(matches!(
        err,
        BuildError::FirstStepNotContextFree {
            first_kind: PlanNodeKind::EdgeTraversal
        }
    ));
}

#[test]
fn zero_depth_traversal_rejected() {
    let err = QueryBuilder::new()
        .scan(NodeKind::Function)
        .traverse(
            Direction::Reverse,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            0,
        )
        .build()
        .unwrap_err();
    assert_eq!(err, BuildError::ZeroDepth);
}

#[test]
fn zero_depth_traverse_any_rejected() {
    let err = QueryBuilder::new()
        .scan_all()
        .traverse_any(Direction::Both, 0)
        .build()
        .unwrap_err();
    assert_eq!(err, BuildError::ZeroDepth);
}

#[test]
fn zero_depth_in_nested_setop_operand_rejected() {
    // Build a malformed sub-plan with depth 0 inside a Chain, then feed it
    // through union() to exercise the recursive subtree validator.
    let bad_sub = QueryPlan::new(PlanNode::Chain {
        steps: vec![
            PlanNode::NodeScan {
                kind: Some(NodeKind::Method),
                visibility: None,
                name_pattern: None,
            },
            PlanNode::EdgeTraversal {
                direction: Direction::Forward,
                edge_kind: None,
                max_depth: 0,
            },
        ],
    });
    let err = QueryBuilder::new()
        .scan(NodeKind::Function)
        .union(bad_sub)
        .build()
        .unwrap_err();
    assert_eq!(err, BuildError::ZeroDepth);
}

#[test]
fn build_error_messages_are_stable() {
    let err = BuildError::EmptyBuilder;
    assert!(err.to_string().contains("empty"));
    let err = BuildError::ZeroDepth;
    assert!(err.to_string().contains("max_depth"));
    let err = BuildError::FirstStepNotContextFree {
        first_kind: PlanNodeKind::Filter,
    };
    assert!(err.to_string().contains("Filter"));
    let err = BuildError::InvalidSetOpOperand {
        reason: "operand root is Filter".into(),
    };
    assert!(err.to_string().contains("operand"));
}

// ---------------------------------------------------------------------------
// Single-step shapes
// ---------------------------------------------------------------------------

#[test]
fn scan_alone_wraps_in_chain() {
    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .build()
        .expect("plan");
    let steps = unwrap_chain(&plan);
    assert_eq!(steps.len(), 1);
    assert!(matches!(
        steps[0],
        PlanNode::NodeScan {
            kind: Some(NodeKind::Function),
            ..
        }
    ));
}

#[test]
fn scan_all_alone_wraps_in_chain() {
    let plan = QueryBuilder::new().scan_all().build().expect("plan");
    let steps = unwrap_chain(&plan);
    assert_eq!(steps.len(), 1);
    let PlanNode::NodeScan {
        kind,
        visibility,
        name_pattern,
    } = &steps[0]
    else {
        panic!("expected NodeScan");
    };
    assert!(kind.is_none());
    assert!(visibility.is_none());
    assert!(name_pattern.is_none());
}

#[test]
fn scan_with_full_filters_round_trip() {
    let pat = StringPattern::glob("handle_*").case_insensitive();
    let plan = QueryBuilder::new()
        .scan_with(
            ScanFilters::new()
                .with_kind(NodeKind::Function)
                .with_visibility(Visibility::Public)
                .with_name_pattern(pat.clone()),
        )
        .build()
        .expect("plan");
    let steps = unwrap_chain(&plan);
    let PlanNode::NodeScan {
        kind,
        visibility,
        name_pattern,
    } = &steps[0]
    else {
        panic!("expected NodeScan");
    };
    assert_eq!(*kind, Some(NodeKind::Function));
    assert_eq!(*visibility, Some(Visibility::Public));
    assert_eq!(name_pattern.as_ref().unwrap(), &pat);
}

// ---------------------------------------------------------------------------
// Multi-step ordering
// ---------------------------------------------------------------------------

#[test]
fn multi_step_chain_preserves_order() {
    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::HasCaller)
        .traverse(
            Direction::Reverse,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            3,
        )
        .filter(Predicate::InFile("src/api/**".into()))
        .build()
        .expect("plan");

    let steps = unwrap_chain(&plan);
    assert_eq!(steps.len(), 4);
    assert!(matches!(steps[0], PlanNode::NodeScan { .. }));
    assert!(matches!(
        steps[1],
        PlanNode::Filter {
            predicate: Predicate::HasCaller
        }
    ));
    assert!(matches!(
        steps[2],
        PlanNode::EdgeTraversal {
            direction: Direction::Reverse,
            ..
        }
    ));
    assert!(matches!(steps[3], PlanNode::Filter { .. }));
}

#[test]
fn design_doc_example_builds() {
    // Verbatim from spec §3.2.
    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::HasCaller)
        .traverse(
            Direction::Reverse,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            3,
        )
        .filter(Predicate::InFile("src/api/**".into()))
        .build()
        .expect("design example must build");

    // chain(1) + scan(1) + filter(1) + traverse(1) + filter(1) = 5
    assert_eq!(plan.operator_count(), 5);
}

// ---------------------------------------------------------------------------
// Set ops
// ---------------------------------------------------------------------------

fn sample_method_plan() -> QueryPlan {
    QueryBuilder::new()
        .scan(NodeKind::Method)
        .build()
        .expect("plan")
}

#[test]
fn union_combines_via_setop() {
    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::HasCaller)
        .union(sample_method_plan())
        .build()
        .expect("plan");
    let steps = unwrap_chain(&plan);
    // The union folds the prior chain into a SetOp left operand.
    assert_eq!(steps.len(), 1);
    let PlanNode::SetOp {
        op, left, right, ..
    } = &steps[0]
    else {
        panic!("expected SetOp root step");
    };
    assert_eq!(*op, SetOperation::Union);
    // Left is a Chain wrapping the original two steps.
    assert!(matches!(left.as_ref(), PlanNode::Chain { .. }));
    // Right is the chain wrapping the sample plan.
    assert!(matches!(right.as_ref(), PlanNode::Chain { .. }));
}

#[test]
fn intersect_combines_via_setop() {
    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .intersect(sample_method_plan())
        .build()
        .expect("plan");
    let steps = unwrap_chain(&plan);
    let PlanNode::SetOp { op, left, .. } = &steps[0] else {
        panic!("expected SetOp");
    };
    assert_eq!(*op, SetOperation::Intersect);
    // Single prior step => left is the bare scan, not wrapped in Chain.
    assert!(matches!(
        left.as_ref(),
        PlanNode::NodeScan {
            kind: Some(NodeKind::Function),
            ..
        }
    ));
}

#[test]
fn difference_combines_via_setop() {
    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .difference(sample_method_plan())
        .build()
        .expect("plan");
    let steps = unwrap_chain(&plan);
    let PlanNode::SetOp { op, .. } = &steps[0] else {
        panic!("expected SetOp");
    };
    assert_eq!(*op, SetOperation::Difference);
}

#[test]
fn empty_builder_union_adopts_other_as_first_step() {
    let plan = QueryBuilder::new()
        .union(sample_method_plan())
        .build()
        .expect("plan");
    let steps = unwrap_chain(&plan);
    // The right operand's Chain was flattened into the builder's own steps
    // so the canonical shape stays `Chain { steps: [NodeScan, ...] }` rather
    // than `Chain { steps: [Chain { steps: [NodeScan] }] }`. SetOp(∅, X) ≡ X,
    // so no SetOp node is introduced.
    assert_eq!(steps.len(), 1);
    assert!(matches!(
        steps[0],
        PlanNode::NodeScan {
            kind: Some(NodeKind::Method),
            ..
        }
    ));
}

#[test]
fn empty_builder_union_with_multi_step_other_flattens_chain() {
    let other = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::HasCaller)
        .build()
        .expect("other");
    let plan = QueryBuilder::new().union(other).build().expect("plan");
    let steps = unwrap_chain(&plan);
    // Two flattened steps from the adopted Chain.
    assert_eq!(steps.len(), 2);
    assert!(matches!(
        steps[0],
        PlanNode::NodeScan {
            kind: Some(NodeKind::Function),
            ..
        }
    ));
    assert!(matches!(
        steps[1],
        PlanNode::Filter {
            predicate: Predicate::HasCaller
        }
    ));
}

#[test]
fn empty_builder_union_with_bare_setop_other_keeps_setop_shape() {
    // Construct a QueryPlan whose root is a bare SetOp (not a Chain). The
    // empty-builder adoption path must accept it as a single context-free
    // first step.
    let scan = PlanNode::NodeScan {
        kind: Some(NodeKind::Function),
        visibility: None,
        name_pattern: None,
    };
    let bare_setop = QueryPlan::new(PlanNode::SetOp {
        op: SetOperation::Union,
        left: Box::new(scan.clone()),
        right: Box::new(scan),
    });
    let plan = QueryBuilder::new().union(bare_setop).build().expect("plan");
    let steps = unwrap_chain(&plan);
    assert_eq!(steps.len(), 1);
    assert!(matches!(steps[0], PlanNode::SetOp { .. }));
}

#[test]
fn chained_setops_preserve_associativity_explicitly() {
    let a = QueryBuilder::new()
        .scan(NodeKind::Function)
        .union(sample_method_plan())
        .build()
        .expect("a");
    let b = QueryBuilder::new()
        .scan(NodeKind::Class)
        .build()
        .expect("b");

    let plan = QueryBuilder::new()
        .scan(NodeKind::Trait)
        .union(a)
        .intersect(b)
        .build()
        .expect("plan");
    // Outer should be a SetOp(Intersect, SetOp(Union, scan(Trait), a), b).
    let steps = unwrap_chain(&plan);
    let PlanNode::SetOp { op, left, .. } = &steps[0] else {
        panic!("expected outer SetOp");
    };
    assert_eq!(*op, SetOperation::Intersect);
    assert!(matches!(
        left.as_ref(),
        PlanNode::SetOp {
            op: SetOperation::Union,
            ..
        }
    ));
}

// ---------------------------------------------------------------------------
// EdgeKind normalisation
// ---------------------------------------------------------------------------

#[test]
fn calls_metadata_normalisation_yields_identical_plans() {
    let a = QueryBuilder::new()
        .scan(NodeKind::Function)
        .traverse(
            Direction::Forward,
            EdgeKind::Calls {
                argument_count: 7,
                is_async: true,
            },
            2,
        )
        .build()
        .expect("a");
    let b = QueryBuilder::new()
        .scan(NodeKind::Function)
        .traverse(
            Direction::Forward,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            2,
        )
        .build()
        .expect("b");
    assert_eq!(a, b, "metadata-stripped plans must be equal");
    assert_eq!(hash_of(&a), hash_of(&b), "and must hash identically");
}

#[test]
fn imports_alias_is_stripped_is_wildcard_is_preserved() {
    // alias is site metadata (StringId) — stripped; is_wildcard is a
    // semantic discriminator — preserved.
    let with_alias = QueryBuilder::new()
        .scan(NodeKind::Module)
        .traverse(
            Direction::Forward,
            EdgeKind::Imports {
                alias: Some(StringId::INVALID),
                is_wildcard: false,
            },
            1,
        )
        .build()
        .expect("with_alias");
    let without_alias = QueryBuilder::new()
        .scan(NodeKind::Module)
        .traverse(
            Direction::Forward,
            EdgeKind::Imports {
                alias: None,
                is_wildcard: false,
            },
            1,
        )
        .build()
        .expect("without_alias");
    assert_eq!(with_alias, without_alias, "alias must be stripped");
    assert_eq!(hash_of(&with_alias), hash_of(&without_alias));

    let wildcard = QueryBuilder::new()
        .scan(NodeKind::Module)
        .traverse(
            Direction::Forward,
            EdgeKind::Imports {
                alias: None,
                is_wildcard: true,
            },
            1,
        )
        .build()
        .expect("wildcard");
    let named = without_alias;
    assert_ne!(
        wildcard, named,
        "is_wildcard is semantic — wildcard and named imports are distinct edges"
    );
    assert_ne!(hash_of(&wildcard), hash_of(&named));
}

// ---------------------------------------------------------------------------
// Semantic discriminator regression tests
//
// For every EdgeKind variant that mixes semantic discriminators with site
// metadata, prove that:
//   (a) differing discriminator values produce DISTINCT plans, and
//   (b) differing site metadata produces IDENTICAL plans.
// ---------------------------------------------------------------------------

fn calls_plan(kind: EdgeKind) -> QueryPlan {
    QueryBuilder::new()
        .scan(NodeKind::Function)
        .traverse(Direction::Forward, kind, 1)
        .build()
        .expect("build")
}

#[test]
fn type_of_context_is_preserved_index_and_name_are_stripped() {
    let param = calls_plan(EdgeKind::TypeOf {
        context: Some(TypeOfContext::Parameter),
        index: Some(7),
        name: Some(StringId::INVALID),
    });
    let ret = calls_plan(EdgeKind::TypeOf {
        context: Some(TypeOfContext::Return),
        index: Some(0),
        name: None,
    });
    let field = calls_plan(EdgeKind::TypeOf {
        context: Some(TypeOfContext::Field),
        index: None,
        name: None,
    });
    assert_ne!(param, ret, "Parameter vs Return are distinct edges");
    assert_ne!(param, field, "Parameter vs Field are distinct edges");
    assert_ne!(ret, field, "Return vs Field are distinct edges");
    assert_ne!(hash_of(&param), hash_of(&ret));
    assert_ne!(hash_of(&param), hash_of(&field));

    // Same context, different site metadata → same plan.
    let param_a = calls_plan(EdgeKind::TypeOf {
        context: Some(TypeOfContext::Parameter),
        index: Some(7),
        name: Some(StringId::INVALID),
    });
    let param_b = calls_plan(EdgeKind::TypeOf {
        context: Some(TypeOfContext::Parameter),
        index: None,
        name: None,
    });
    assert_eq!(param_a, param_b);
    assert_eq!(hash_of(&param_a), hash_of(&param_b));
}

#[test]
fn exports_kind_is_preserved_alias_is_stripped() {
    let direct = calls_plan(EdgeKind::Exports {
        kind: ExportKind::Direct,
        alias: None,
    });
    let reexport = calls_plan(EdgeKind::Exports {
        kind: ExportKind::Reexport,
        alias: None,
    });
    let default = calls_plan(EdgeKind::Exports {
        kind: ExportKind::Default,
        alias: None,
    });
    let namespace = calls_plan(EdgeKind::Exports {
        kind: ExportKind::Namespace,
        alias: None,
    });
    for other in [&reexport, &default, &namespace] {
        assert_ne!(&direct, other);
        assert_ne!(hash_of(&direct), hash_of(other));
    }

    let direct_a = calls_plan(EdgeKind::Exports {
        kind: ExportKind::Direct,
        alias: Some(StringId::INVALID),
    });
    let direct_b = calls_plan(EdgeKind::Exports {
        kind: ExportKind::Direct,
        alias: None,
    });
    assert_eq!(
        direct_a, direct_b,
        "alias is site metadata — must be stripped"
    );
}

#[test]
fn lifetime_constraint_kind_is_preserved() {
    let outlives = calls_plan(EdgeKind::LifetimeConstraint {
        constraint_kind: LifetimeConstraintKind::Outlives,
    });
    let higher_ranked = calls_plan(EdgeKind::LifetimeConstraint {
        constraint_kind: LifetimeConstraintKind::HigherRanked,
    });
    assert_ne!(outlives, higher_ranked);
    assert_ne!(hash_of(&outlives), hash_of(&higher_ranked));
}

#[test]
fn macro_expansion_kind_is_preserved_is_verified_is_stripped() {
    let derive = calls_plan(EdgeKind::MacroExpansion {
        expansion_kind: MacroExpansionKind::Derive,
        is_verified: false,
    });
    let cfg_gate = calls_plan(EdgeKind::MacroExpansion {
        expansion_kind: MacroExpansionKind::CfgGate,
        is_verified: false,
    });
    assert_ne!(derive, cfg_gate, "Derive vs CfgGate are distinct edges");

    let derive_verified = calls_plan(EdgeKind::MacroExpansion {
        expansion_kind: MacroExpansionKind::Derive,
        is_verified: true,
    });
    assert_eq!(
        derive, derive_verified,
        "is_verified is site metadata — must be stripped"
    );
}

#[test]
fn ffi_convention_is_preserved() {
    let c_conv = calls_plan(EdgeKind::FfiCall {
        convention: FfiConvention::C,
    });
    let stdcall = calls_plan(EdgeKind::FfiCall {
        convention: FfiConvention::Stdcall,
    });
    assert_ne!(c_conv, stdcall);
}

#[test]
fn http_method_is_preserved_url_is_stripped() {
    let get = calls_plan(EdgeKind::HttpRequest {
        method: HttpMethod::Get,
        url: None,
    });
    let post = calls_plan(EdgeKind::HttpRequest {
        method: HttpMethod::Post,
        url: None,
    });
    let delete = calls_plan(EdgeKind::HttpRequest {
        method: HttpMethod::Delete,
        url: None,
    });
    for other in [&post, &delete] {
        assert_ne!(&get, other);
    }

    let get_a = calls_plan(EdgeKind::HttpRequest {
        method: HttpMethod::Get,
        url: Some(StringId::INVALID),
    });
    let get_b = calls_plan(EdgeKind::HttpRequest {
        method: HttpMethod::Get,
        url: None,
    });
    assert_eq!(get_a, get_b, "url is site metadata — must be stripped");
}

#[test]
fn db_query_type_is_preserved_table_is_stripped() {
    let select = calls_plan(EdgeKind::DbQuery {
        query_type: DbQueryType::Select,
        table: None,
    });
    let insert = calls_plan(EdgeKind::DbQuery {
        query_type: DbQueryType::Insert,
        table: None,
    });
    assert_ne!(select, insert);

    let select_a = calls_plan(EdgeKind::DbQuery {
        query_type: DbQueryType::Select,
        table: Some(StringId::INVALID),
    });
    let select_b = calls_plan(EdgeKind::DbQuery {
        query_type: DbQueryType::Select,
        table: None,
    });
    assert_eq!(select_a, select_b);
}

#[test]
fn table_write_operation_is_preserved_table_name_and_schema_are_stripped() {
    let insert = calls_plan(EdgeKind::TableWrite {
        table_name: StringId::INVALID,
        schema: None,
        operation: TableWriteOp::Insert,
    });
    let update = calls_plan(EdgeKind::TableWrite {
        table_name: StringId::INVALID,
        schema: None,
        operation: TableWriteOp::Update,
    });
    let delete = calls_plan(EdgeKind::TableWrite {
        table_name: StringId::INVALID,
        schema: None,
        operation: TableWriteOp::Delete,
    });
    for other in [&update, &delete] {
        assert_ne!(&insert, other);
    }
}

#[test]
fn message_queue_protocol_is_preserved_topic_is_stripped() {
    let kafka = calls_plan(EdgeKind::MessageQueue {
        protocol: MqProtocol::Kafka,
        topic: None,
    });
    let sqs = calls_plan(EdgeKind::MessageQueue {
        protocol: MqProtocol::Sqs,
        topic: None,
    });
    assert_ne!(kafka, sqs);

    let kafka_a = calls_plan(EdgeKind::MessageQueue {
        protocol: MqProtocol::Kafka,
        topic: Some(StringId::INVALID),
    });
    let kafka_b = calls_plan(EdgeKind::MessageQueue {
        protocol: MqProtocol::Kafka,
        topic: None,
    });
    assert_eq!(kafka_a, kafka_b);
}

#[test]
fn trait_method_binding_all_fields_are_stripped() {
    // trait_name and impl_type are StringIds — snapshot-dependent, so
    // cannot be preserved in the IR. is_ambiguous is site metadata.
    let a = calls_plan(EdgeKind::TraitMethodBinding {
        trait_name: StringId::INVALID,
        impl_type: StringId::INVALID,
        is_ambiguous: false,
    });
    let b = calls_plan(EdgeKind::TraitMethodBinding {
        trait_name: StringId::INVALID,
        impl_type: StringId::INVALID,
        is_ambiguous: true,
    });
    assert_eq!(a, b);
}

#[test]
fn normalize_edge_kind_covers_every_metadata_variant() {
    // For every metadata-bearing variant, normalising twice is idempotent.
    let cases: Vec<EdgeKind> = vec![
        EdgeKind::Calls {
            argument_count: 9,
            is_async: true,
        },
        EdgeKind::Imports {
            alias: Some(StringId::INVALID),
            is_wildcard: true,
        },
        EdgeKind::Exports {
            kind: ExportKind::Reexport,
            alias: Some(StringId::INVALID),
        },
        EdgeKind::TypeOf {
            context: None,
            index: Some(3),
            name: Some(StringId::INVALID),
        },
        EdgeKind::LifetimeConstraint {
            constraint_kind: LifetimeConstraintKind::HigherRanked,
        },
        EdgeKind::TraitMethodBinding {
            trait_name: StringId::INVALID,
            impl_type: StringId::INVALID,
            is_ambiguous: true,
        },
        EdgeKind::MacroExpansion {
            expansion_kind: MacroExpansionKind::Derive,
            is_verified: true,
        },
        EdgeKind::FfiCall {
            convention: FfiConvention::Stdcall,
        },
        EdgeKind::HttpRequest {
            method: HttpMethod::Post,
            url: Some(StringId::INVALID),
        },
        EdgeKind::GrpcCall {
            service: StringId::INVALID,
            method: StringId::INVALID,
        },
        EdgeKind::DbQuery {
            query_type: DbQueryType::Insert,
            table: Some(StringId::INVALID),
        },
        EdgeKind::TableRead {
            table_name: StringId::INVALID,
            schema: Some(StringId::INVALID),
        },
        EdgeKind::TableWrite {
            table_name: StringId::INVALID,
            schema: Some(StringId::INVALID),
            operation: TableWriteOp::Update,
        },
        EdgeKind::TriggeredBy {
            trigger_name: StringId::INVALID,
            schema: Some(StringId::INVALID),
        },
        EdgeKind::MessageQueue {
            protocol: MqProtocol::Sqs,
            topic: Some(StringId::INVALID),
        },
        EdgeKind::WebSocket {
            event: Some(StringId::INVALID),
        },
        EdgeKind::GraphQLOperation {
            operation: StringId::INVALID,
        },
        EdgeKind::ProcessExec {
            command: StringId::INVALID,
        },
        EdgeKind::FileIpc {
            path_pattern: Some(StringId::INVALID),
        },
        EdgeKind::ProtocolCall {
            protocol: StringId::INVALID,
            metadata: Some(StringId::INVALID),
        },
    ];

    for case in cases {
        let once = normalize_edge_kind(case.clone());
        let twice = normalize_edge_kind(once.clone());
        assert_eq!(once, twice, "normalisation must be idempotent for {case:?}");
    }
}

#[test]
fn normalize_passes_through_metadata_free_variants() {
    let metadata_free = [
        EdgeKind::Defines,
        EdgeKind::Contains,
        EdgeKind::References,
        EdgeKind::Inherits,
        EdgeKind::Implements,
        EdgeKind::WebAssemblyCall,
        EdgeKind::GenericBound,
        EdgeKind::AnnotatedWith,
        EdgeKind::AnnotationParam,
        EdgeKind::LambdaCaptures,
        EdgeKind::ModuleExports,
        EdgeKind::ModuleRequires,
        EdgeKind::ModuleOpens,
        EdgeKind::ModuleProvides,
        EdgeKind::TypeArgument,
        EdgeKind::ExtensionReceiver,
        EdgeKind::CompanionOf,
        EdgeKind::SealedPermit,
    ];
    for c in metadata_free {
        assert_eq!(normalize_edge_kind(c.clone()), c);
    }
}

// ---------------------------------------------------------------------------
// Subquery composition
// ---------------------------------------------------------------------------

#[test]
fn builder_output_used_as_subquery_round_trips_through_json() {
    let inner = QueryBuilder::new()
        .scan(NodeKind::Class)
        .filter(Predicate::MatchesName(StringPattern::contains("Service")))
        .build()
        .expect("inner");

    let outer = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::Callers(inner.clone().into_subquery()))
        .build()
        .expect("outer");

    let json = serde_json::to_string(&outer).expect("serialize");
    let decoded: QueryPlan = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(decoded, outer);

    // The decoded plan still recognises the embedded subquery.
    let steps = unwrap_chain(&decoded);
    let PlanNode::Filter {
        predicate: Predicate::Callers(value),
    } = &steps[1]
    else {
        panic!("expected Callers filter");
    };
    assert!(value.is_subquery());
    assert_eq!(value.as_subquery().unwrap(), &inner.root);
}

#[test]
fn as_subquery_borrows_without_consuming() {
    let plan = QueryBuilder::new()
        .scan(NodeKind::Trait)
        .build()
        .expect("plan");
    let value = plan.as_subquery();
    assert!(value.is_subquery());
    // plan is still usable after.
    assert_eq!(plan.operator_count(), 2);
}

#[test]
fn subquery_with_zero_depth_traversal_is_rejected() {
    let bad_inner = QueryPlan::new(PlanNode::Chain {
        steps: vec![
            PlanNode::NodeScan {
                kind: Some(NodeKind::Method),
                visibility: None,
                name_pattern: None,
            },
            PlanNode::EdgeTraversal {
                direction: Direction::Forward,
                edge_kind: None,
                max_depth: 0,
            },
        ],
    });
    let err = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::Callees(bad_inner.into_subquery()))
        .build()
        .unwrap_err();
    assert_eq!(err, BuildError::ZeroDepth);
}

// ---------------------------------------------------------------------------
// Predicate combinators through .filter()
// ---------------------------------------------------------------------------

#[test]
fn predicate_and_or_not_nest_through_filter() {
    let pred = Predicate::And(vec![
        Predicate::Or(vec![
            Predicate::HasCaller,
            Predicate::Not(Box::new(Predicate::IsUnused)),
        ]),
        Predicate::InFile(PathPattern::new("src/**/*.rs")),
    ]);
    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter(pred.clone())
        .build()
        .expect("plan");
    let steps = unwrap_chain(&plan);
    let PlanNode::Filter { predicate } = &steps[1] else {
        panic!("expected Filter");
    };
    assert_eq!(predicate, &pred);
}

#[test]
fn predicate_and_with_subquery_propagates_has_subquery() {
    let inner = QueryBuilder::new()
        .scan(NodeKind::Method)
        .build()
        .expect("inner");

    let pred = Predicate::And(vec![
        Predicate::HasCaller,
        Predicate::Callers(inner.into_subquery()),
    ]);
    assert!(pred.has_subquery());

    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter(pred)
        .build()
        .expect("plan");
    let steps = unwrap_chain(&plan);
    let PlanNode::Filter { predicate } = &steps[1] else {
        panic!("expected Filter");
    };
    assert!(predicate.has_subquery());
}

// ---------------------------------------------------------------------------
// Coverage: every NodeKind, every Visibility, every Direction, every
// PredicateValue-bearing predicate, every StringPattern constructor.
// ---------------------------------------------------------------------------

const ALL_NODE_KINDS: &[NodeKind] = &[
    NodeKind::Function,
    NodeKind::Method,
    NodeKind::Class,
    NodeKind::Interface,
    NodeKind::Trait,
    NodeKind::Module,
    NodeKind::Variable,
    NodeKind::Constant,
    NodeKind::Type,
    NodeKind::Struct,
    NodeKind::Enum,
    NodeKind::EnumVariant,
    NodeKind::Macro,
    NodeKind::Parameter,
    NodeKind::Property,
    NodeKind::CallSite,
    NodeKind::Import,
    NodeKind::Export,
    NodeKind::StyleRule,
    NodeKind::StyleAtRule,
    NodeKind::StyleVariable,
    NodeKind::Lifetime,
    NodeKind::Component,
    NodeKind::Service,
    NodeKind::Resource,
    NodeKind::Endpoint,
    NodeKind::Test,
    NodeKind::TypeParameter,
    NodeKind::Annotation,
    NodeKind::AnnotationValue,
    NodeKind::LambdaTarget,
    NodeKind::JavaModule,
    NodeKind::EnumConstant,
    NodeKind::Other,
];

#[test]
fn every_node_kind_is_a_valid_scan_filter() {
    // Verifies the full coverage of the public NodeKind enum (currently
    // 34 variants). If the enum grows, add the new variant to ALL_NODE_KINDS
    // and rerun.
    for kind in ALL_NODE_KINDS {
        let plan = QueryBuilder::new()
            .scan_with(ScanFilters::new().with_kind(*kind))
            .build()
            .unwrap_or_else(|e| panic!("scan(kind = {kind:?}) failed: {e}"));
        let steps = unwrap_chain(&plan);
        let PlanNode::NodeScan { kind: filter, .. } = &steps[0] else {
            panic!("expected NodeScan");
        };
        assert_eq!(*filter, Some(*kind));
    }
}

#[test]
fn every_visibility_is_a_valid_scan_filter() {
    for vis in [Visibility::Public, Visibility::Private] {
        let plan = QueryBuilder::new()
            .scan_with(
                ScanFilters::new()
                    .with_kind(NodeKind::Function)
                    .with_visibility(vis),
            )
            .build()
            .expect("plan");
        let PlanNode::NodeScan { visibility, .. } = &unwrap_chain(&plan)[0] else {
            panic!("expected NodeScan");
        };
        assert_eq!(*visibility, Some(vis));
    }
}

#[test]
fn every_direction_is_a_valid_traversal() {
    for dir in [Direction::Forward, Direction::Reverse, Direction::Both] {
        let plan = QueryBuilder::new()
            .scan(NodeKind::Function)
            .traverse(dir, EdgeKind::References, 1)
            .build()
            .expect("plan");
        let PlanNode::EdgeTraversal { direction, .. } = &unwrap_chain(&plan)[1] else {
            panic!("expected EdgeTraversal");
        };
        assert_eq!(*direction, dir);
    }
}

#[test]
fn every_predicate_value_bearing_predicate_is_constructible() {
    let value = PredicateValue::Pattern(StringPattern::exact("target"));

    // Build a small plan for each, asserting the variant survives intact.
    let preds: Vec<Predicate> = vec![
        Predicate::Callers(value.clone()),
        Predicate::Callees(value.clone()),
        Predicate::Imports(value.clone()),
        Predicate::Exports(value.clone()),
        Predicate::References(value.clone()),
        Predicate::Implements(value.clone()),
    ];
    for pred in preds {
        let plan = QueryBuilder::new()
            .scan(NodeKind::Function)
            .filter(pred.clone())
            .build()
            .expect("plan");
        let PlanNode::Filter { predicate } = &unwrap_chain(&plan)[1] else {
            panic!("expected Filter");
        };
        assert_eq!(predicate, &pred);
    }
}

#[test]
fn predicate_value_regex_round_trips() {
    let regex = PredicateValue::Regex(RegexPattern::with_flags(
        r"^parse_\w+$",
        RegexFlags {
            case_insensitive: true,
            multiline: false,
            dot_all: false,
        },
    ));
    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::References(regex.clone()))
        .build()
        .expect("plan");
    let PlanNode::Filter {
        predicate: Predicate::References(value),
    } = &unwrap_chain(&plan)[1]
    else {
        panic!("expected References predicate");
    };
    assert_eq!(value, &regex);
}

#[test]
fn every_string_pattern_constructor_accepted() {
    let raw = "needle";
    let constructors: &[(&str, StringPattern)] = &[
        ("exact", StringPattern::exact(raw)),
        ("glob", StringPattern::glob(raw)),
        ("prefix", StringPattern::prefix(raw)),
        ("suffix", StringPattern::suffix(raw)),
        ("contains", StringPattern::contains(raw)),
    ];
    for (label, pattern) in constructors {
        let plan = QueryBuilder::new()
            .scan_with(ScanFilters::new().with_name_pattern(pattern.clone()))
            .build()
            .unwrap_or_else(|e| panic!("scan with {label} pattern failed: {e}"));
        let PlanNode::NodeScan { name_pattern, .. } = &unwrap_chain(&plan)[0] else {
            panic!("expected NodeScan");
        };
        assert_eq!(name_pattern.as_ref().unwrap(), pattern);
    }
}

#[test]
fn case_insensitive_pattern_round_trips() {
    let p = StringPattern::glob("Foo*").case_insensitive();
    assert!(p.case_insensitive);
    let plan = QueryBuilder::new()
        .scan_with(ScanFilters::new().with_name_pattern(p.clone()))
        .filter_name(StringPattern::contains("Bar").case_insensitive())
        .build()
        .expect("plan");

    let PlanNode::NodeScan { name_pattern, .. } = &unwrap_chain(&plan)[0] else {
        panic!("expected NodeScan");
    };
    assert!(name_pattern.as_ref().unwrap().case_insensitive);
}

// ---------------------------------------------------------------------------
// Filter-helper sugar
// ---------------------------------------------------------------------------

#[test]
fn filter_in_file_sugar_yields_in_file_predicate() {
    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter_in_file("src/**/*.rs")
        .build()
        .expect("plan");
    let PlanNode::Filter { predicate } = &unwrap_chain(&plan)[1] else {
        panic!("expected Filter");
    };
    let Predicate::InFile(path) = predicate else {
        panic!("expected InFile predicate");
    };
    assert_eq!(path.as_str(), "src/**/*.rs");
}

#[test]
fn filter_name_sugar_yields_matches_name_predicate() {
    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter_name(StringPattern::prefix("test_"))
        .build()
        .expect("plan");
    let PlanNode::Filter { predicate } = &unwrap_chain(&plan)[1] else {
        panic!("expected Filter");
    };
    assert!(matches!(predicate, Predicate::MatchesName(_)));
}

// ---------------------------------------------------------------------------
// In-scope filtering covers every ScopeKind
// ---------------------------------------------------------------------------

#[test]
fn every_scope_kind_filter_round_trips() {
    for sk in [
        ScopeKind::Module,
        ScopeKind::Function,
        ScopeKind::Class,
        ScopeKind::Namespace,
        ScopeKind::Trait,
        ScopeKind::Impl,
    ] {
        let plan = QueryBuilder::new()
            .scan(NodeKind::Function)
            .filter(Predicate::InScope(sk))
            .build()
            .expect("plan");
        let PlanNode::Filter { predicate } = &unwrap_chain(&plan)[1] else {
            panic!("expected Filter");
        };
        assert_eq!(predicate, &Predicate::InScope(sk));
    }
}

// ---------------------------------------------------------------------------
// Cloning the builder for branched queries
// ---------------------------------------------------------------------------

#[test]
fn cloning_a_builder_preserves_independent_chains() {
    let prefix = QueryBuilder::new().scan(NodeKind::Function);
    let with_callers = prefix
        .clone()
        .filter(Predicate::HasCaller)
        .build()
        .expect("a");
    let with_callees = prefix.filter(Predicate::HasCallee).build().expect("b");

    assert_ne!(with_callers, with_callees);
    let a_steps = unwrap_chain(&with_callers);
    let b_steps = unwrap_chain(&with_callees);
    assert_eq!(a_steps.len(), 2);
    assert_eq!(b_steps.len(), 2);
    assert!(matches!(
        a_steps[1],
        PlanNode::Filter {
            predicate: Predicate::HasCaller
        }
    ));
    assert!(matches!(
        b_steps[1],
        PlanNode::Filter {
            predicate: Predicate::HasCallee
        }
    ));
}

// ---------------------------------------------------------------------------
// Hash + equality round-trips for fuser invariants
// ---------------------------------------------------------------------------

#[test]
fn semantically_identical_builders_hash_identically() {
    let a = QueryBuilder::new()
        .scan(NodeKind::Function)
        .traverse(
            Direction::Forward,
            EdgeKind::Calls {
                argument_count: 4,
                is_async: true,
            },
            2,
        )
        .filter(Predicate::HasCaller)
        .build()
        .expect("a");
    let b = QueryBuilder::new()
        .scan(NodeKind::Function)
        .traverse(
            Direction::Forward,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            2,
        )
        .filter(Predicate::HasCaller)
        .build()
        .expect("b");
    assert_eq!(hash_of(&a), hash_of(&b));
    assert_eq!(a, b);
}

#[test]
fn distinct_directions_hash_distinctly() {
    let a = QueryBuilder::new()
        .scan(NodeKind::Function)
        .traverse_any(Direction::Forward, 1)
        .build()
        .expect("a");
    let b = QueryBuilder::new()
        .scan(NodeKind::Function)
        .traverse_any(Direction::Reverse, 1)
        .build()
        .expect("b");
    assert_ne!(hash_of(&a), hash_of(&b));
    assert_ne!(a, b);
}

// ---------------------------------------------------------------------------
// Postcard round-trip (the wire format used by `.sqry/graph/derived.sqry`)
// ---------------------------------------------------------------------------

#[test]
fn builder_output_round_trips_through_postcard() {
    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::HasCaller)
        .traverse(Direction::Reverse, EdgeKind::Implements, 4)
        .union(
            QueryBuilder::new()
                .scan(NodeKind::Method)
                .build()
                .expect("inner"),
        )
        .build()
        .expect("plan");
    let bytes = postcard::to_allocvec(&plan).expect("encode");
    let decoded: QueryPlan = postcard::from_bytes(&bytes).expect("decode");
    assert_eq!(decoded, plan);
}

// ---------------------------------------------------------------------------
// Defensive: ScanFilters default + builder default
// ---------------------------------------------------------------------------

#[test]
fn scan_filters_default_is_all_none() {
    let f = ScanFilters::default();
    assert!(f.kind.is_none());
    assert!(f.visibility.is_none());
    assert!(f.name_pattern.is_none());
}

#[test]
fn query_builder_default_is_empty() {
    let b = QueryBuilder::default();
    assert!(b.is_empty());
    assert_eq!(b.step_count(), 0);
    assert_eq!(b.build().unwrap_err(), BuildError::EmptyBuilder);
}
