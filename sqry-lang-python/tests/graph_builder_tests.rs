/// Integration tests for Python `GraphBuilder`
/// Run with: `SQRY_USE_GRAPH=1` cargo test -p sqry-lang-python --test `graph_builder_tests`
use sqry_core::graph::{GraphBuilder, unified::build::StagingGraph};
use sqry_lang_python::relations::PythonGraphBuilder;
use sqry_test_support::graph_helpers::{
    assert_call_edge_has_span, assert_has_call_edge, collect_call_edges,
};
use std::collections::HashSet;
use std::path::Path;
use tree_sitter::Parser;

fn parse_python(source: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .expect("Error loading Python grammar");
    parser.parse(source, None).expect("Error parsing")
}

// Test for dotted call expression handling (regression prevention)
#[test]
fn graph_builder_handles_dotted_call_expressions() {
    let source = r"
class MyClass:
    def method(self):
        return 42

def call_method():
    obj = MyClass()
    result = obj.method()
    return result
";

    let tree = parse_python(source);
    let file = Path::new("test_dotted_calls.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // Should have call edge from call_method to MyClass.method
    // Target should be "method", NOT "obj" (regression test)
    assert_has_call_edge(&staging, "call_method", "method");
}

// Test for module.function() pattern
#[test]
fn graph_builder_handles_module_function_calls() {
    let source = r"
import math

def calculate():
    result = math.sqrt(16)
    return result
";

    let tree = parse_python(source);
    let file = Path::new("test_module_calls.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // Should have call edge to "sqrt", NOT "math"
    assert_has_call_edge(&staging, "calculate", "sqrt");
}

#[test]
fn graph_builder_extracts_function_calls() {
    let source = r"
def helper():
    return 1

def entry():
    result = helper()
    return result
";

    let tree = parse_python(source);
    let file = Path::new("test_graph_builder_extracts_function_calls.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let call_edges = collect_call_edges(&staging);
    let call_edge_count = call_edges.len();
    assert!(
        !call_edges.is_empty(),
        "Expected at least 1 call edge, got {call_edge_count}"
    );

    assert_has_call_edge(&staging, "entry", "helper");
    assert_call_edge_has_span(&staging, "entry", "helper");
}

#[test]
fn graph_builder_handles_method_calls() {
    let source = r"
class Widget:
    def helper(self):
        return 42

    def process(self):
        return self.helper()
";

    let tree = parse_python(source);
    let file = Path::new("test_methods.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert_has_call_edge(&staging, "Widget.process", "Widget.helper");
}

#[test]
fn graph_builder_handles_nested_functions() {
    let source = r"
def outer():
    def inner():
        return 1
    return inner()
";

    let tree = parse_python(source);
    let file = Path::new("test_nested.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");
}

#[test]
fn graph_builder_handles_async_functions() {
    let source = r#"
async def fetch_data():
    return "data"

async def process():
    data = await fetch_data()
    return data
"#;

    let tree = parse_python(source);
    let file = Path::new("test_async.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let async_call = assert_has_call_edge(&staging, "process", "fetch_data");
    assert!(
        async_call.is_async,
        "Expected async call to have is_async=true metadata"
    );
}

#[test]
fn graph_builder_handles_top_level_calls() {
    let source = r#"
def bootstrap():
    return "initialized"

# Top-level call
result = bootstrap()
"#;

    let tree = parse_python(source);
    let file = Path::new("test_toplevel.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert_has_call_edge(&staging, "<module>", "bootstrap");
}

#[test]
fn graph_builder_handles_chained_method_calls() {
    let source = r"
class Calculator:
    def add(self, x):
        return x + 1

    def multiply(self, x):
        return self.add(x) * 2
";

    let tree = parse_python(source);
    let file = Path::new("test_chained.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert_has_call_edge(&staging, "Calculator.multiply", "Calculator.add");
}

#[test]
fn graph_builder_extracts_multiple_calls_in_function() {
    let source = r"
def helper1():
    return 1

def helper2():
    return 2

def caller():
    a = helper1()
    b = helper2()
    return a + b
";

    let tree = parse_python(source);
    let file = Path::new("test_multiple.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    let call_edges = collect_call_edges(&staging);
    let call_edge_count = call_edges.len();

    // Should have at least 2 call sites: caller->helper1, caller->helper2
    assert!(
        call_edge_count >= 2,
        "Expected at least 2 call edges, got {call_edge_count}"
    );

    assert_has_call_edge(&staging, "caller", "helper1");
    assert_has_call_edge(&staging, "caller", "helper2");
}

#[test]
fn graph_builder_handles_constructor_calls() {
    let source = r"
class Widget:
    def __init__(self):
        pass

def create_widget():
    return Widget()
";

    let tree = parse_python(source);
    let file = Path::new("test_constructor.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert_has_call_edge(&staging, "create_widget", "Widget");
}

#[test]
fn graph_builder_handles_lambda_calls() {
    let source = r"
def process(func):
    return func()

def caller():
    result = process(lambda: 42)
    return result
";

    let tree = parse_python(source);
    let file = Path::new("test_lambda.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert_has_call_edge(&staging, "caller", "process");
}

#[test]
fn graph_builder_handles_class_method_calls() {
    let source = r"
class MyClass:
    @classmethod
    def create(cls):
        return cls()

    def use_classmethod(self):
        return MyClass.create()
";

    let tree = parse_python(source);
    let file = Path::new("test_classmethod.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");

    assert_has_call_edge(&staging, "use_classmethod", "create");
}

#[test]
fn graph_builder_detects_python_imports() {
    let source = r#"
import os
import sys
from pathlib import Path
from typing import List, Dict
import numpy as np

def process():
    return os.path.exists("/tmp")
"#;

    let tree = parse_python(source);
    let file = Path::new("test_graph_builder_detects_python_imports.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph");
}

// ============================================================================
// Export Tests (__all__)
// ============================================================================

use sqry_core::graph::unified::build::StagingOp;
use sqry_core::graph::unified::edge::EdgeKind;

/// Helper function to count exports in staging graph
fn count_export_edges(staging: &StagingGraph) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Exports { .. },
                    ..
                }
            )
        })
        .count()
}

/// Helper function to count inherits edges in staging graph
fn count_inherits_edges(staging: &StagingGraph) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Inherits,
                    ..
                }
            )
        })
        .count()
}

#[test]
fn graph_builder_extracts_all_exports_simple_list() {
    let source = r#"
__all__ = ['foo', 'bar', 'baz']

def foo():
    pass

def bar():
    pass

class baz:
    pass
"#;

    let tree = parse_python(source);
    let file = Path::new("test_all_exports.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // Should have 3 export edges for foo, bar, baz
    let export_count = count_export_edges(&staging);
    assert_eq!(
        export_count, 3,
        "Expected 3 export edges for __all__ = ['foo', 'bar', 'baz'], got {export_count}"
    );
}

#[test]
fn graph_builder_extracts_all_exports_double_quotes() {
    let source = r#"
__all__ = ["helper", "process"]

def helper():
    return 1

def process():
    return helper()
"#;

    let tree = parse_python(source);
    let file = Path::new("test_all_exports_double_quotes.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let export_count = count_export_edges(&staging);
    assert_eq!(
        export_count, 2,
        "Expected 2 export edges for __all__ with double quotes, got {export_count}"
    );
}

#[test]
fn graph_builder_extracts_all_exports_tuple() {
    let source = r#"
__all__ = ('alpha', 'beta')

def alpha():
    pass

def beta():
    pass
"#;

    let tree = parse_python(source);
    let file = Path::new("test_all_exports_tuple.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let export_count = count_export_edges(&staging);
    assert_eq!(
        export_count, 2,
        "Expected 2 export edges for __all__ tuple, got {export_count}"
    );
}

#[test]
fn graph_builder_ignores_non_all_assignments() {
    let source = r#"
__version__ = '1.0.0'
exports = ['a', 'b']  # Not __all__

def real_func():
    pass
"#;

    let tree = parse_python(source);
    let file = Path::new("test_non_all_assignments.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // When __all__ is not defined, public functions are exported by default (Python semantics)
    // real_func is a public function (no leading underscore), so it should be exported
    let export_count = count_export_edges(&staging);
    assert_eq!(
        export_count, 1,
        "Expected 1 export edge for public function when __all__ is not defined, got {export_count}"
    );
}

#[test]
fn graph_builder_handles_empty_all() {
    let source = r#"
__all__ = []

def internal_func():
    pass
"#;

    let tree = parse_python(source);
    let file = Path::new("test_empty_all.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let export_count = count_export_edges(&staging);
    assert_eq!(
        export_count, 0,
        "Expected 0 export edges for empty __all__, got {export_count}"
    );
}

// ============================================================================
// Inheritance Tests (OOP)
// ============================================================================

#[test]
fn graph_builder_extracts_single_inheritance() {
    let source = r#"
class Parent:
    pass

class Child(Parent):
    pass
"#;

    let tree = parse_python(source);
    let file = Path::new("test_single_inheritance.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let inherits_count = count_inherits_edges(&staging);
    assert_eq!(
        inherits_count, 1,
        "Expected 1 inherits edge for Child(Parent), got {inherits_count}"
    );
}

#[test]
fn graph_builder_extracts_multiple_inheritance() {
    let source = r#"
class Mixin1:
    pass

class Mixin2:
    pass

class Base:
    pass

class Derived(Base, Mixin1, Mixin2):
    pass
"#;

    let tree = parse_python(source);
    let file = Path::new("test_multiple_inheritance.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let inherits_count = count_inherits_edges(&staging);
    assert_eq!(
        inherits_count, 3,
        "Expected 3 inherits edges for Derived(Base, Mixin1, Mixin2), got {inherits_count}"
    );
}

#[test]
fn graph_builder_extracts_qualified_base_class() {
    let source = r#"
import collections.abc

class MySequence(collections.abc.Sequence):
    pass
"#;

    let tree = parse_python(source);
    let file = Path::new("test_qualified_base.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let inherits_count = count_inherits_edges(&staging);
    assert_eq!(
        inherits_count, 1,
        "Expected 1 inherits edge for qualified base class, got {inherits_count}"
    );
}

#[test]
fn graph_builder_extracts_abc_inheritance() {
    let source = r#"
from abc import ABC, abstractmethod

class AbstractBase(ABC):
    @abstractmethod
    def do_something(self):
        pass

class Concrete(AbstractBase):
    def do_something(self):
        return 42
"#;

    let tree = parse_python(source);
    let file = Path::new("test_abc_inheritance.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // AbstractBase inherits from ABC, Concrete inherits from AbstractBase
    let inherits_count = count_inherits_edges(&staging);
    assert_eq!(
        inherits_count, 2,
        "Expected 2 inherits edges (AbstractBase->ABC, Concrete->AbstractBase), got {inherits_count}"
    );
}

#[test]
fn graph_builder_handles_generic_base_class() {
    let source = r#"
from typing import Generic, TypeVar

T = TypeVar('T')

class Container(Generic[T]):
    def __init__(self, item: T):
        self.item = item
"#;

    let tree = parse_python(source);
    let file = Path::new("test_generic_base.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // Container inherits from Generic[T] - we should capture Generic as base
    let inherits_count = count_inherits_edges(&staging);
    assert_eq!(
        inherits_count, 1,
        "Expected 1 inherits edge for Generic[T] base class, got {inherits_count}"
    );
}

#[test]
fn graph_builder_handles_metaclass() {
    let source = r#"
from abc import ABCMeta

class WithMeta(metaclass=ABCMeta):
    pass
"#;

    let tree = parse_python(source);
    let file = Path::new("test_metaclass.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // metaclass=ABCMeta is NOT inheritance, should be 0
    let inherits_count = count_inherits_edges(&staging);
    assert_eq!(
        inherits_count, 0,
        "Expected 0 inherits edges for metaclass=ABCMeta (not inheritance), got {inherits_count}"
    );
}

#[test]
fn graph_builder_handles_class_without_bases() {
    let source = r#"
class StandaloneClass:
    def method(self):
        return 42
"#;

    let tree = parse_python(source);
    let file = Path::new("test_no_base.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let inherits_count = count_inherits_edges(&staging);
    assert_eq!(
        inherits_count, 0,
        "Expected 0 inherits edges for class without explicit base, got {inherits_count}"
    );
}

#[test]
fn graph_builder_handles_nested_classes_with_inheritance() {
    let source = r#"
class Outer:
    class InnerBase:
        pass

    class InnerChild(InnerBase):
        pass
"#;

    let tree = parse_python(source);
    let file = Path::new("test_nested_inheritance.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let inherits_count = count_inherits_edges(&staging);
    assert_eq!(
        inherits_count, 1,
        "Expected 1 inherits edge for InnerChild(InnerBase), got {inherits_count}"
    );
}

#[test]
fn graph_builder_combined_exports_and_inheritance() {
    let source = r#"
__all__ = ['Animal', 'Dog', 'Cat', 'make_sound']

class Animal:
    def sound(self):
        pass

class Dog(Animal):
    def sound(self):
        return "woof"

class Cat(Animal):
    def sound(self):
        return "meow"

def make_sound(animal):
    return animal.sound()
"#;

    let tree = parse_python(source);
    let file = Path::new("test_combined.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // 4 exports: Animal, Dog, Cat, make_sound
    let export_count = count_export_edges(&staging);
    assert_eq!(
        export_count, 4,
        "Expected 4 export edges, got {export_count}"
    );

    // 2 inherits: Dog->Animal, Cat->Animal
    let inherits_count = count_inherits_edges(&staging);
    assert_eq!(
        inherits_count, 2,
        "Expected 2 inherits edges, got {inherits_count}"
    );
}

#[test]
fn graph_builder_handles_dataclass_inheritance() {
    let source = r#"
from dataclasses import dataclass

@dataclass
class BaseData:
    name: str

@dataclass
class ExtendedData(BaseData):
    value: int
"#;

    let tree = parse_python(source);
    let file = Path::new("test_dataclass_inheritance.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let inherits_count = count_inherits_edges(&staging);
    assert_eq!(
        inherits_count, 1,
        "Expected 1 inherits edge for dataclass inheritance, got {inherits_count}"
    );
}

#[test]
fn graph_builder_handles_exception_inheritance() {
    let source = r#"
class CustomError(Exception):
    pass

class SpecificError(CustomError):
    pass

class AnotherError(ValueError, CustomError):
    pass
"#;

    let tree = parse_python(source);
    let file = Path::new("test_exception_inheritance.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // CustomError->Exception, SpecificError->CustomError, AnotherError->ValueError, AnotherError->CustomError
    let inherits_count = count_inherits_edges(&staging);
    assert_eq!(
        inherits_count, 4,
        "Expected 4 inherits edges for exception hierarchy, got {inherits_count}"
    );
}

// ============================================================================
// FFI Tests (ctypes, cffi, native extensions)
// ============================================================================

/// Helper to count FfiCall edges from staging operations
fn count_ffi_edges(staging: &StagingGraph) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::FfiCall { .. },
                    ..
                }
            )
        })
        .count()
}

/// Helper to collect FfiCall edge (source, target) pairs for duplicate detection.
/// Returns Vec of (caller_name, callee_name) tuples.
fn collect_ffi_edge_pairs(staging: &StagingGraph) -> Vec<(String, String)> {
    use sqry_core::graph::unified::build::StagingOp;
    use sqry_core::graph::unified::edge::EdgeKind;
    use sqry_test_support::graph_helpers::build_node_name_lookup;

    let name_lookup = build_node_name_lookup(staging);

    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge {
                source,
                target,
                kind: EdgeKind::FfiCall { .. },
                ..
            } = op
            {
                let source_name = name_lookup
                    .get(&source.index())
                    .cloned()
                    .unwrap_or_else(|| format!("{source:?}"));
                let target_name = name_lookup
                    .get(&target.index())
                    .cloned()
                    .unwrap_or_else(|| format!("{target:?}"));
                Some((source_name, target_name))
            } else {
                None
            }
        })
        .collect()
}

// Test for the helper function itself (test-first approach)
#[test]
fn test_collect_ffi_edge_pairs_helper() {
    let source = r#"
import ctypes

def load_library():
    lib = ctypes.CDLL('libfoo.so')
    return lib
"#;

    let tree = parse_python(source);
    let file = Path::new("test_helper.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // Test the helper function
    let ffi_pairs = collect_ffi_edge_pairs(&staging);

    // Should have at least one FFI edge
    assert!(!ffi_pairs.is_empty(), "Expected at least one FFI edge pair");

    // Check for duplicates - convert to HashSet
    let unique_pairs: HashSet<_> = ffi_pairs.iter().collect();
    assert_eq!(
        ffi_pairs.len(),
        unique_pairs.len(),
        "Found duplicate FFI edges: {ffi_pairs:?}"
    );

    // Verify the pair contains expected native module (simplified name)
    let has_native = ffi_pairs
        .iter()
        .any(|(_, target)| target.contains("native::"));
    assert!(
        has_native,
        "Expected FFI edge to native module, got: {ffi_pairs:?}"
    );
}

#[test]
fn graph_builder_detects_ctypes_cdll() {
    let source = r#"
import ctypes

def load_library():
    lib = ctypes.CDLL('libfoo.so')
    return lib
"#;

    let tree = parse_python(source);
    let file = Path::new("test_ctypes_cdll.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // Should have exactly 1 FFI call: ctypes.CDLL
    let ffi_count = count_ffi_edges(&staging);
    assert_eq!(
        ffi_count, 1,
        "Expected exactly 1 FfiCall edge for ctypes.CDLL, got {ffi_count}"
    );

    // Check for no duplicate edges
    let ffi_pairs = collect_ffi_edge_pairs(&staging);
    let unique_pairs: HashSet<_> = ffi_pairs.iter().collect();
    assert_eq!(
        ffi_pairs.len(),
        unique_pairs.len(),
        "Found duplicate FfiCall edges: {ffi_pairs:?}"
    );
}

#[test]
fn graph_builder_detects_ctypes_windll() {
    let source = r#"
import ctypes

def load_win_library():
    kernel32 = ctypes.WinDLL('kernel32')
    return kernel32
"#;

    let tree = parse_python(source);
    let file = Path::new("test_ctypes_windll.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // Should have exactly 1 FFI call: ctypes.WinDLL
    let ffi_count = count_ffi_edges(&staging);
    assert_eq!(
        ffi_count, 1,
        "Expected exactly 1 FfiCall edge for ctypes.WinDLL, got {ffi_count}"
    );

    // Check for no duplicate edges
    let ffi_pairs = collect_ffi_edge_pairs(&staging);
    let unique_pairs: HashSet<_> = ffi_pairs.iter().collect();
    assert_eq!(
        ffi_pairs.len(),
        unique_pairs.len(),
        "Found duplicate FfiCall edges: {ffi_pairs:?}"
    );
}

#[test]
fn graph_builder_detects_ctypes_cdll_load_library() {
    let source = r#"
from ctypes import cdll

def load_via_cdll():
    lib = cdll.LoadLibrary('libbar.so')
    return lib
"#;

    let tree = parse_python(source);
    let file = Path::new("test_ctypes_cdll_loadlibrary.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // Should have exactly 1 FFI call: cdll.LoadLibrary
    let ffi_count = count_ffi_edges(&staging);
    assert_eq!(
        ffi_count, 1,
        "Expected exactly 1 FfiCall edge for cdll.LoadLibrary, got {ffi_count}"
    );

    // Check for no duplicate edges
    let ffi_pairs = collect_ffi_edge_pairs(&staging);
    let unique_pairs: HashSet<_> = ffi_pairs.iter().collect();
    assert_eq!(
        ffi_pairs.len(),
        unique_pairs.len(),
        "Found duplicate FfiCall edges: {ffi_pairs:?}"
    );
}

#[test]
fn graph_builder_detects_direct_cdll_import() {
    let source = r#"
from ctypes import CDLL

def load_direct():
    lib = CDLL('libdirect.so')
    return lib
"#;

    let tree = parse_python(source);
    let file = Path::new("test_direct_cdll.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // Should have exactly 1 FFI call: CDLL (direct import)
    let ffi_count = count_ffi_edges(&staging);
    assert_eq!(
        ffi_count, 1,
        "Expected exactly 1 FfiCall edge for direct CDLL import, got {ffi_count}"
    );

    // Check for no duplicate edges
    let ffi_pairs = collect_ffi_edge_pairs(&staging);
    let unique_pairs: HashSet<_> = ffi_pairs.iter().collect();
    assert_eq!(
        ffi_pairs.len(),
        unique_pairs.len(),
        "Found duplicate FfiCall edges: {ffi_pairs:?}"
    );
}

#[test]
fn graph_builder_detects_cffi_dlopen() {
    let source = r#"
from cffi import FFI

def load_cffi():
    ffi = FFI()
    lib = ffi.dlopen('libcffi.so')
    return lib
"#;

    let tree = parse_python(source);
    let file = Path::new("test_cffi_dlopen.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // Should have exactly 1 FFI call: ffi.dlopen
    let ffi_count = count_ffi_edges(&staging);
    assert_eq!(
        ffi_count, 1,
        "Expected exactly 1 FfiCall edge for cffi dlopen, got {ffi_count}"
    );

    // Check for no duplicate edges
    let ffi_pairs = collect_ffi_edge_pairs(&staging);
    let unique_pairs: HashSet<_> = ffi_pairs.iter().collect();
    assert_eq!(
        ffi_pairs.len(),
        unique_pairs.len(),
        "Found duplicate FfiCall edges: {ffi_pairs:?}"
    );
}

#[test]
fn graph_builder_detects_multiple_ffi_calls() {
    let source = r#"
import ctypes

def load_multiple():
    lib1 = ctypes.CDLL('lib1.so')
    lib2 = ctypes.CDLL('lib2.so')
    win = ctypes.WinDLL('kernel32')
    return lib1, lib2, win
"#;

    let tree = parse_python(source);
    let file = Path::new("test_multiple_ffi.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // Should have exactly 3 FFI calls: CDLL x2 + WinDLL
    let ffi_count = count_ffi_edges(&staging);
    assert_eq!(
        ffi_count, 3,
        "Expected exactly 3 FfiCall edges for multiple library loads, got {ffi_count}"
    );

    // All 3 should be unique (lib1, lib2, kernel32)
    let ffi_pairs = collect_ffi_edge_pairs(&staging);
    let unique_pairs: HashSet<_> = ffi_pairs.iter().collect();
    assert_eq!(
        ffi_pairs.len(),
        unique_pairs.len(),
        "Found duplicate FfiCall edges: {ffi_pairs:?}"
    );

    // Verify we have the expected targets
    assert_eq!(
        unique_pairs.len(),
        3,
        "Expected 3 unique FFI targets (lib1, lib2, kernel32), got {} from {ffi_pairs:?}",
        unique_pairs.len()
    );
}

#[test]
fn graph_builder_detects_numpy_native_import() {
    let source = r#"
import numpy as np

def process_array():
    arr = np.array([1, 2, 3])
    return arr.sum()
"#;

    let tree = parse_python(source);
    let file = Path::new("test_numpy_import.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // Should have exactly 1 FFI call: numpy import (known C extension)
    let ffi_count = count_ffi_edges(&staging);
    assert_eq!(
        ffi_count, 1,
        "Expected exactly 1 FfiCall edge for numpy import (known C extension), got {ffi_count}"
    );

    // Check for no duplicate edges
    let ffi_pairs = collect_ffi_edge_pairs(&staging);
    let unique_pairs: HashSet<_> = ffi_pairs.iter().collect();
    assert_eq!(
        ffi_pairs.len(),
        unique_pairs.len(),
        "Found duplicate FfiCall edges: {ffi_pairs:?}"
    );
}

#[test]
fn graph_builder_detects_private_c_module_import() {
    let source = r#"
import _sqlite3

def get_connection():
    return _sqlite3.connect(':memory:')
"#;

    let tree = parse_python(source);
    let file = Path::new("test_private_c_module.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // Should have exactly 1 FFI call: _sqlite3 (private C module)
    let ffi_count = count_ffi_edges(&staging);
    assert_eq!(
        ffi_count, 1,
        "Expected exactly 1 FfiCall edge for _sqlite3 (private C module), got {ffi_count}"
    );

    // Check for no duplicate edges
    let ffi_pairs = collect_ffi_edge_pairs(&staging);
    let unique_pairs: HashSet<_> = ffi_pairs.iter().collect();
    assert_eq!(
        ffi_pairs.len(),
        unique_pairs.len(),
        "Found duplicate FfiCall edges: {ffi_pairs:?}"
    );
}

#[test]
fn graph_builder_detects_pandas_native_import() {
    let source = r#"
import pandas as pd

def create_dataframe():
    return pd.DataFrame({'a': [1, 2], 'b': [3, 4]})
"#;

    let tree = parse_python(source);
    let file = Path::new("test_pandas_import.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // Should have exactly 1 FFI call: pandas import (known C extension)
    let ffi_count = count_ffi_edges(&staging);
    assert_eq!(
        ffi_count, 1,
        "Expected exactly 1 FfiCall edge for pandas import (known C extension), got {ffi_count}"
    );

    // Check for no duplicate edges
    let ffi_pairs = collect_ffi_edge_pairs(&staging);
    let unique_pairs: HashSet<_> = ffi_pairs.iter().collect();
    assert_eq!(
        ffi_pairs.len(),
        unique_pairs.len(),
        "Found duplicate FfiCall edges: {ffi_pairs:?}"
    );
}

#[test]
fn graph_builder_skips_pure_python_imports() {
    let source = r#"
import json
import os
import sys
from collections import defaultdict
from typing import List, Dict
"#;

    let tree = parse_python(source);
    let file = Path::new("test_pure_python_imports.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // These are pure Python modules - no FFI edges should be created
    // Note: json, os, sys have some C components but their main modules are Python
    // We don't create FFI edges for standard library imports
    let ffi_count = count_ffi_edges(&staging);
    assert_eq!(
        ffi_count, 0,
        "Expected 0 FfiCall edges for pure Python standard library imports, got {ffi_count}"
    );
}

#[test]
fn graph_builder_ffi_top_level_ctypes() {
    let source = r#"
import ctypes

# Top-level library loading
libc = ctypes.CDLL('libc.so.6')
"#;

    let tree = parse_python(source);
    let file = Path::new("test_top_level_ctypes.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // Should have exactly 1 FFI call: ctypes.CDLL (top-level)
    let ffi_count = count_ffi_edges(&staging);
    assert_eq!(
        ffi_count, 1,
        "Expected exactly 1 FfiCall edge for top-level ctypes.CDLL, got {ffi_count}"
    );

    // Check for no duplicate edges
    let ffi_pairs = collect_ffi_edge_pairs(&staging);
    let unique_pairs: HashSet<_> = ffi_pairs.iter().collect();
    assert_eq!(
        ffi_pairs.len(),
        unique_pairs.len(),
        "Found duplicate FfiCall edges: {ffi_pairs:?}"
    );
}

#[test]
fn graph_builder_combined_ffi_and_regular_calls() {
    let source = r#"
import ctypes

def helper():
    return 42

def process():
    # Regular call
    value = helper()

    # FFI call
    lib = ctypes.CDLL('libmath.so')

    return value
"#;

    let tree = parse_python(source);
    let file = Path::new("test_combined_ffi_calls.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // Should have exactly 1 FFI call: ctypes.CDLL
    let ffi_count = count_ffi_edges(&staging);
    assert_eq!(
        ffi_count, 1,
        "Expected exactly 1 FfiCall edge, got {ffi_count}"
    );

    // Check for no duplicate edges
    let ffi_pairs = collect_ffi_edge_pairs(&staging);
    let unique_pairs: HashSet<_> = ffi_pairs.iter().collect();
    assert_eq!(
        ffi_pairs.len(),
        unique_pairs.len(),
        "Found duplicate FfiCall edges: {ffi_pairs:?}"
    );

    // Should also have regular call edge
    assert_has_call_edge(&staging, "process", "helper");
}

// ============================================================================
// FFI False Positive Prevention Tests
// ============================================================================

#[test]
fn graph_builder_no_ffi_for_generic_load_library_call() {
    // This test verifies we don't create FFI edges for arbitrary .LoadLibrary calls
    // that aren't related to ctypes
    let source = r#"
class MyLoader:
    def LoadLibrary(self, name):
        return f"loaded {name}"

def load_something():
    loader = MyLoader()
    # This should NOT be an FFI call - it's just a method named LoadLibrary
    result = loader.LoadLibrary('mymodule')
    return result
"#;

    let tree = parse_python(source);
    let file = Path::new("test_no_ffi_generic_loadlibrary.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // Should NOT have FFI edge for generic .LoadLibrary
    let ffi_count = count_ffi_edges(&staging);
    assert_eq!(
        ffi_count, 0,
        "Expected 0 FfiCall edges for generic LoadLibrary method (not ctypes), got {ffi_count}"
    );
}

#[test]
fn graph_builder_no_ffi_for_generic_dlopen_call() {
    // This test verifies we don't create FFI edges for arbitrary .dlopen calls
    // that aren't related to cffi
    let source = r#"
class CustomDynamicLoader:
    def dlopen(self, path):
        return f"opened {path}"

def load_dynamic():
    loader = CustomDynamicLoader()
    # This should NOT be an FFI call - it's just a method named dlopen
    result = loader.dlopen('/path/to/lib.so')
    return result
"#;

    let tree = parse_python(source);
    let file = Path::new("test_no_ffi_generic_dlopen.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // Should NOT have FFI edge for generic .dlopen
    let ffi_count = count_ffi_edges(&staging);
    assert_eq!(
        ffi_count, 0,
        "Expected 0 FfiCall edges for generic dlopen method (not cffi), got {ffi_count}"
    );
}

// ============================================================================
// HTTP Route Endpoint Detection Tests (Flask/FastAPI)
// ============================================================================

use sqry_core::graph::unified::node::NodeKind;

/// Helper to collect Endpoint node qualified names from staging graph operations.
fn collect_endpoint_names(staging: &StagingGraph) -> Vec<String> {
    use sqry_test_support::graph_helpers::{build_node_name_lookup, build_string_lookup};

    let strings = build_string_lookup(staging);
    let _names = build_node_name_lookup(staging);

    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode { entry, .. } = op
                && entry.kind == NodeKind::Endpoint
            {
                let name_idx = entry.qualified_name.unwrap_or(entry.name).index();
                strings.get(&name_idx).cloned()
            } else {
                None
            }
        })
        .collect()
}

/// Helper to count Contains edges in staging graph operations.
fn count_contains_edges(staging: &StagingGraph) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Contains,
                    ..
                }
            )
        })
        .count()
}

#[test]
fn graph_builder_detects_flask_route_decorator_default_get() {
    let source = r#"
from flask import Flask

app = Flask(__name__)

@app.route('/api/users')
def get_users():
    return []
"#;

    let tree = parse_python(source);
    let file = Path::new("test_flask_route.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let endpoints = collect_endpoint_names(&staging);
    assert_eq!(
        endpoints.len(),
        1,
        "Expected 1 Endpoint node for @app.route('/api/users'), got {}: {endpoints:?}",
        endpoints.len()
    );
    assert_eq!(
        endpoints[0], "route::GET::/api/users",
        "Expected endpoint name 'route::GET::/api/users', got '{}'",
        endpoints[0]
    );
}

#[test]
fn graph_builder_detects_flask_route_decorator_with_methods_post() {
    let source = r#"
from flask import Flask

app = Flask(__name__)

@app.route('/api/users', methods=['POST'])
def create_user():
    return {"id": 1}
"#;

    let tree = parse_python(source);
    let file = Path::new("test_flask_route_post.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let endpoints = collect_endpoint_names(&staging);
    assert_eq!(
        endpoints.len(),
        1,
        "Expected 1 Endpoint node for @app.route with methods=['POST'], got {}: {endpoints:?}",
        endpoints.len()
    );
    assert_eq!(
        endpoints[0], "route::POST::/api/users",
        "Expected endpoint name 'route::POST::/api/users', got '{}'",
        endpoints[0]
    );
}

#[test]
fn graph_builder_detects_flask_method_decorators() {
    let source = r#"
from flask import Flask

app = Flask(__name__)

@app.get('/api/users')
def list_users():
    return []

@app.post('/api/users')
def create_user():
    return {"id": 1}

@app.put('/api/users/1')
def update_user():
    return {"id": 1}

@app.delete('/api/users/1')
def delete_user():
    return None

@app.patch('/api/users/1')
def patch_user():
    return {"id": 1}
"#;

    let tree = parse_python(source);
    let file = Path::new("test_flask_methods.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let endpoints = collect_endpoint_names(&staging);
    assert_eq!(
        endpoints.len(),
        5,
        "Expected 5 Endpoint nodes for GET/POST/PUT/DELETE/PATCH, got {}: {endpoints:?}",
        endpoints.len()
    );

    let endpoint_set: HashSet<&str> = endpoints.iter().map(String::as_str).collect();
    assert!(
        endpoint_set.contains("route::GET::/api/users"),
        "Missing GET endpoint: {endpoints:?}"
    );
    assert!(
        endpoint_set.contains("route::POST::/api/users"),
        "Missing POST endpoint: {endpoints:?}"
    );
    assert!(
        endpoint_set.contains("route::PUT::/api/users/1"),
        "Missing PUT endpoint: {endpoints:?}"
    );
    assert!(
        endpoint_set.contains("route::DELETE::/api/users/1"),
        "Missing DELETE endpoint: {endpoints:?}"
    );
    assert!(
        endpoint_set.contains("route::PATCH::/api/users/1"),
        "Missing PATCH endpoint: {endpoints:?}"
    );
}

#[test]
fn graph_builder_detects_fastapi_router_decorators() {
    let source = r#"
from fastapi import APIRouter

router = APIRouter()

@router.get('/api/items')
async def list_items():
    return []

@router.post('/api/items')
async def create_item():
    return {"id": 1}
"#;

    let tree = parse_python(source);
    let file = Path::new("test_fastapi_router.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let endpoints = collect_endpoint_names(&staging);
    assert_eq!(
        endpoints.len(),
        2,
        "Expected 2 Endpoint nodes for FastAPI router, got {}: {endpoints:?}",
        endpoints.len()
    );

    let endpoint_set: HashSet<&str> = endpoints.iter().map(String::as_str).collect();
    assert!(
        endpoint_set.contains("route::GET::/api/items"),
        "Missing GET endpoint: {endpoints:?}"
    );
    assert!(
        endpoint_set.contains("route::POST::/api/items"),
        "Missing POST endpoint: {endpoints:?}"
    );
}

#[test]
fn graph_builder_detects_blueprint_route_decorator() {
    let source = r#"
from flask import Blueprint

blueprint = Blueprint('api', __name__)

@blueprint.route('/health')
def health_check():
    return {"status": "ok"}
"#;

    let tree = parse_python(source);
    let file = Path::new("test_blueprint_route.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let endpoints = collect_endpoint_names(&staging);
    assert_eq!(
        endpoints.len(),
        1,
        "Expected 1 Endpoint node for blueprint.route, got {}: {endpoints:?}",
        endpoints.len()
    );
    assert_eq!(
        endpoints[0], "route::GET::/health",
        "Expected 'route::GET::/health', got '{}'",
        endpoints[0]
    );
}

#[test]
fn graph_builder_creates_contains_edge_for_route_endpoint() {
    let source = r#"
from flask import Flask

app = Flask(__name__)

@app.get('/api/users')
def get_users():
    return []
"#;

    let tree = parse_python(source);
    let file = Path::new("test_route_contains.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // Should have at least one Contains edge (endpoint -> function)
    let contains_count = count_contains_edges(&staging);
    assert!(
        contains_count >= 1,
        "Expected at least 1 Contains edge for route endpoint -> function, got {contains_count}"
    );
}

#[test]
fn graph_builder_no_endpoint_for_non_route_decorators() {
    let source = r#"
from functools import wraps

def my_decorator(f):
    @wraps(f)
    def wrapper(*args, **kwargs):
        return f(*args, **kwargs)
    return wrapper

@my_decorator
def regular_function():
    return 42
"#;

    let tree = parse_python(source);
    let file = Path::new("test_no_endpoint_for_non_route.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let endpoints = collect_endpoint_names(&staging);
    assert_eq!(
        endpoints.len(),
        0,
        "Expected 0 Endpoint nodes for non-route decorators, got {}: {endpoints:?}",
        endpoints.len()
    );
}

#[test]
fn graph_builder_no_endpoint_for_property_decorator() {
    let source = r#"
class MyClass:
    @property
    def name(self):
        return self._name
"#;

    let tree = parse_python(source);
    let file = Path::new("test_no_endpoint_for_property.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let endpoints = collect_endpoint_names(&staging);
    assert_eq!(
        endpoints.len(),
        0,
        "Expected 0 Endpoint nodes for @property decorator, got {}: {endpoints:?}",
        endpoints.len()
    );
}

#[test]
fn graph_builder_route_with_double_quoted_path() {
    let source = r#"
from flask import Flask

app = Flask(__name__)

@app.get("/api/items")
def list_items():
    return []
"#;

    let tree = parse_python(source);
    let file = Path::new("test_route_double_quotes.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let endpoints = collect_endpoint_names(&staging);
    assert_eq!(
        endpoints.len(),
        1,
        "Expected 1 Endpoint node for route with double-quoted path, got {}: {endpoints:?}",
        endpoints.len()
    );
    assert_eq!(
        endpoints[0], "route::GET::/api/items",
        "Expected 'route::GET::/api/items', got '{}'",
        endpoints[0]
    );
}

#[test]
fn graph_builder_no_endpoint_for_undecorated_function() {
    let source = r#"
def plain_function():
    return 42
"#;

    let tree = parse_python(source);
    let file = Path::new("test_no_endpoint_plain.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let endpoints = collect_endpoint_names(&staging);
    assert_eq!(
        endpoints.len(),
        0,
        "Expected 0 Endpoint nodes for plain function, got {}: {endpoints:?}",
        endpoints.len()
    );
}

#[test]
fn graph_builder_route_does_not_affect_existing_function_node() {
    // Verify that adding a route endpoint doesn't prevent the function
    // from being created as a regular function node
    let source = r#"
from flask import Flask

app = Flask(__name__)

@app.get('/api/users')
def get_users():
    return []

def helper():
    return get_users()
"#;

    let tree = parse_python(source);
    let file = Path::new("test_route_preserves_function.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // Should have the endpoint
    let endpoints = collect_endpoint_names(&staging);
    assert_eq!(
        endpoints.len(),
        1,
        "Expected 1 Endpoint node, got {}: {endpoints:?}",
        endpoints.len()
    );

    // Should still have regular call edge (helper -> get_users)
    assert_has_call_edge(&staging, "helper", "get_users");
}

#[test]
fn graph_builder_multiple_routes_on_different_functions() {
    let source = r#"
from flask import Flask

app = Flask(__name__)

@app.get('/api/users')
def list_users():
    return []

@app.post('/api/users')
def create_user():
    return {"id": 1}

@app.get('/api/health')
def health():
    return {"status": "ok"}
"#;

    let tree = parse_python(source);
    let file = Path::new("test_multiple_routes.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    let endpoints = collect_endpoint_names(&staging);
    assert_eq!(
        endpoints.len(),
        3,
        "Expected 3 Endpoint nodes, got {}: {endpoints:?}",
        endpoints.len()
    );

    let endpoint_set: HashSet<&str> = endpoints.iter().map(String::as_str).collect();
    assert!(endpoint_set.contains("route::GET::/api/users"));
    assert!(endpoint_set.contains("route::POST::/api/users"));
    assert!(endpoint_set.contains("route::GET::/api/health"));
}

// Note: ctypes.cdll.msvcrt and ctypes.windll.kernel32 style attribute access
// patterns are not detected because they're attribute access, not function calls.
// Detecting them would require tracking attribute access on specific objects,
// which is beyond the current scope. The important FFI patterns (CDLL(), WinDLL(),
// etc.) are covered by other tests.

#[test]
fn graph_builder_no_ffi_for_unrelated_methods() {
    // Ensure we don't create FFI edges for completely unrelated method calls
    // that happen to have similar-looking names
    let source = r#"
class Database:
    def open(self):
        pass

class FileSystem:
    def dlsym(self, name):
        pass

def test_methods():
    db = Database()
    db.open()  # Not FFI

    fs = FileSystem()
    fs.dlsym('symbol')  # Not FFI - just happens to have dlsym name
"#;

    let tree = parse_python(source);
    let file = Path::new("test_no_ffi_unrelated.py");
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();

    builder
        .build_graph(&tree, source.as_bytes(), file, &mut staging)
        .expect("build graph should succeed");

    // Should NOT have any FFI edges - these are just regular method calls
    let ffi_count = count_ffi_edges(&staging);
    assert_eq!(
        ffi_count, 0,
        "Expected 0 FfiCall edges for unrelated methods, got {ffi_count}"
    );

    // But should have regular call edges
    let call_sites = collect_call_edges(&staging);
    assert!(
        !call_sites.is_empty(),
        "Expected regular call sites for method calls"
    );
}
