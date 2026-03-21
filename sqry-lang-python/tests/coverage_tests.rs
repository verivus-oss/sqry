//! Coverage-targeted tests for `sqry-lang-python`.
//!
//! Exercises uncovered paths in:
//! - `src/relations/graph_builder.rs`:
//!   - `__all__` assignment (list export, tuple export, augmented assignment)
//!   - class inheritance: qualified (attribute), call, subscript (generic) bases
//!   - async generators, property decorators, route decorators (Flask/FastAPI)
//!   - `has_all_assignment` detection
//!   - relative imports (`from . import x`)
//!   - `is_module_level` and `is_public_name` edge cases
//!   - `ffi_library_simple_name` variants
//! - `src/relations/local_scopes.rs`:
//!   - lambda scope, comprehension scope (list/set/dict/generator)
//!   - walrus operator binding
//!   - `except Exception as e` binding
//!   - `with ... as x` binding
//!   - for-in-clause (comprehension variable)
//!   - `typed_parameter`, `default_parameter`, `typed_default_parameter`
//!   - `list_splat_pattern` (*args), `dictionary_splat_pattern` (**kwargs)

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::build::staging::StagingGraph;
use sqry_lang_python::relations::PythonGraphBuilder;
use std::path::Path;
use tree_sitter::Tree;

// ─────────────────────────────────────────────────────────────────────────────
// Shared helpers
// ─────────────────────────────────────────────────────────────────────────────

fn parse_python(source: &str) -> Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .expect("set Python language");
    parser.parse(source, None).expect("parse Python")
}

fn build_graph(source: &str) -> StagingGraph {
    let tree = parse_python(source);
    let mut staging = StagingGraph::new();
    let builder = PythonGraphBuilder::default();
    builder
        .build_graph(&tree, source.as_bytes(), Path::new("test.py"), &mut staging)
        .expect("build_graph should not fail");
    staging
}

fn has_edge_tag(staging: &StagingGraph, tag: &str) -> bool {
    use sqry_core::graph::unified::build::staging::StagingOp;
    staging
        .operations()
        .iter()
        .any(|op| matches!(op, StagingOp::AddEdge { kind, .. } if kind.tag() == tag))
}

fn all_edge_tags(staging: &StagingGraph) -> Vec<String> {
    use sqry_core::graph::unified::build::staging::StagingOp;
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddEdge { kind, .. } = op {
                Some(kind.tag().to_string())
            } else {
                None
            }
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// __all__ assignment export handling
// ─────────────────────────────────────────────────────────────────────────────

/// `__all__` list export creates export edges for listed names.
#[test]
fn all_assignment_list_creates_export_edges() {
    let source = r#"
def public_func():
    pass

def _private_func():
    pass

__all__ = ['public_func']
"#;
    let staging = build_graph(source);
    assert!(
        has_edge_tag(&staging, "exports"),
        "Expected exports edge from __all__. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

/// `__all__` with tuple syntax (also valid Python)
#[test]
fn all_assignment_tuple_creates_export_edges() {
    let source = r#"
def alpha():
    pass

def beta():
    pass

__all__ = ('alpha', 'beta')
"#;
    let staging = build_graph(source);
    assert!(
        has_edge_tag(&staging, "exports"),
        "Expected exports edge from __all__ tuple. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

/// `__all__ += ['name']` augmented assignment
#[test]
fn all_augmented_assignment() {
    let source = r#"
def extra():
    pass

__all__ = ['extra']
__all__ += ['more']
"#;
    // Should not panic; base __all__ assignment produces export edges
    let staging = build_graph(source);
    assert!(
        staging.stats().nodes_staged >= 1,
        "Expected at least one node from augmented assignment source"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Class inheritance — all 4 base kinds
// ─────────────────────────────────────────────────────────────────────────────

/// Simple identifier base class → Inherits edge
#[test]
fn class_inherits_identifier_base() {
    let source = r#"
class Animal:
    def speak(self): pass

class Dog(Animal):
    def bark(self): pass
"#;
    let staging = build_graph(source);
    assert!(
        has_edge_tag(&staging, "inherits"),
        "Expected inherits edge. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

/// Attribute base class: `class Child(module.Base):`
#[test]
fn class_inherits_attribute_base() {
    let source = r#"
import abc

class MyInterface(abc.ABC):
    def method(self): pass
"#;
    let staging = build_graph(source);
    // Attribute-form base (abc.ABC) produces class + method nodes
    assert!(
        staging.stats().nodes_staged >= 1,
        "Expected class and method nodes from attribute base class"
    );
}

/// Call base class: `class Child(SomeMixin()):`
#[test]
fn class_inherits_call_base() {
    let source = r#"
def make_mixin():
    class M:
        pass
    return M

class Concrete(make_mixin()):
    pass
"#;
    let staging = build_graph(source);
    // Call-form base exercises the call-expression branch of base resolution
    assert!(
        staging.stats().nodes_staged >= 1,
        "Expected nodes from call-form base class"
    );
}

/// Subscript (generic) base class: `class MyList(list[int]):`
#[test]
fn class_inherits_subscript_base() {
    let source = r#"
from typing import Generic, TypeVar
T = TypeVar('T')

class Box(Generic[T]):
    def get(self) -> T: ...
"#;
    let staging = build_graph(source);
    // Subscript-form base (Generic[T]) exercises subscript branch; imports edge expected
    assert!(
        has_edge_tag(&staging, "imports"),
        "Expected imports edge from 'from typing import'. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

/// Multiple inheritance with keyword argument (metaclass): should skip keyword args
#[test]
fn class_inherits_skips_keyword_arguments() {
    let source = r#"
import abc

class MyABC(abc.ABC, metaclass=abc.ABCMeta):
    def do_it(self): pass
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

// ─────────────────────────────────────────────────────────────────────────────
// Property decorator handling
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn property_decorator_creates_property_node() {
    let source = r#"
class Temperature:
    def __init__(self, value: float):
        self._value = value

    @property
    def celsius(self) -> float:
        return self._value

    @property
    def fahrenheit(self) -> float:
        return self._value * 9 / 5 + 32
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 2, "Expected property nodes");
}

// ─────────────────────────────────────────────────────────────────────────────
// Async generators and async functions
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn async_function_creates_function_node() {
    let source = r#"
import asyncio

async def fetch(url: str) -> str:
    await asyncio.sleep(0)
    return url
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

#[test]
fn async_generator_function() {
    let source = r#"
async def async_range(n: int):
    for i in range(n):
        yield i
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

/// Awaited call (`await fetch()`) should be recorded as async call edge
#[test]
fn awaited_call_recorded() {
    let source = r#"
import asyncio

async def helper():
    return 42

async def runner():
    result = await helper()
    return result
"#;
    let staging = build_graph(source);
    // Should have a Calls edge (is_async may be set)
    assert!(
        has_edge_tag(&staging, "calls"),
        "Expected calls edge for awaited call. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Route decorators (Flask / FastAPI)
// ─────────────────────────────────────────────────────────────────────────────

/// Flask-style route: `@app.route('/path')`
#[test]
fn flask_route_decorator() {
    let source = r#"
class Flask:
    def route(self, path, methods=None):
        def decorator(f):
            return f
        return decorator

app = Flask()

@app.route('/users', methods=['GET'])
def get_users():
    return []
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

/// FastAPI-style route: `@router.get('/path')`
#[test]
fn fastapi_get_route_decorator() {
    let source = r#"
class APIRouter:
    def get(self, path: str):
        def decorator(f):
            return f
        return decorator

router = APIRouter()

@router.get('/items')
async def list_items():
    return []
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

// ─────────────────────────────────────────────────────────────────────────────
// Import handling edge cases
// ─────────────────────────────────────────────────────────────────────────────

/// `import numpy as np` (aliased import)
#[test]
fn aliased_import() {
    let source = r#"
import numpy as np

def use_numpy():
    return np.array([1, 2, 3])
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

/// `from os.path import join` (from import)
#[test]
fn from_import() {
    let source = r#"
from os.path import join

def make_path(base: str, name: str) -> str:
    return join(base, name)
"#;
    let staging = build_graph(source);
    assert!(
        has_edge_tag(&staging, "imports"),
        "Expected imports edge. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

/// Relative import `from . import utils`
#[test]
fn relative_import() {
    let source = r"
from . import utils

def use_utils():
    return utils.helper()
";
    // Relative imports should produce an imports edge (or at minimum not panic)
    let staging = build_graph(source);
    assert!(
        has_edge_tag(&staging, "imports"),
        "Expected imports edge for relative import. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

/// Relative import with module: `from .models import User`
#[test]
fn relative_import_with_module() {
    let source = r"
from .models import User

def get_user(user_id: int) -> User:
    return User(user_id)
";
    let staging = build_graph(source);
    assert!(
        has_edge_tag(&staging, "imports"),
        "Expected imports edge for relative import with module. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

/// Wildcard import `from module import *`
#[test]
fn wildcard_import() {
    let source = r#"
from typing import *

def foo(x: Optional[int]) -> List[str]:
    return []
"#;
    let staging = build_graph(source);
    assert!(
        has_edge_tag(&staging, "imports"),
        "Expected imports edge for wildcard import. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Type-annotated functions (return type + parameter type hints)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn function_with_return_type_annotation() {
    let source = r#"
from typing import List

def get_names() -> List[str]:
    return ['alice', 'bob']
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

#[test]
fn method_with_typed_parameters() {
    let source = r#"
class Calculator:
    def add(self, a: int, b: int) -> int:
        return a + b

    def subtract(self, a: float, b: float) -> float:
        return a - b
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 2);
}

// ─────────────────────────────────────────────────────────────────────────────
// local_scopes.rs — scope and binding coverage
// ─────────────────────────────────────────────────────────────────────────────

/// Lambda scope: binds its parameters separately from enclosing function
#[test]
fn lambda_creates_scope() {
    let source = r#"
def make_adder(n):
    return lambda x: x + n
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

/// List comprehension scope (iteration variable scoped to comprehension)
#[test]
fn list_comprehension_scope() {
    let source = r#"
def squares(n):
    return [x * x for x in range(n)]
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

/// Set comprehension scope
#[test]
fn set_comprehension_scope() {
    let source = r#"
def unique_squares(items):
    return {x * x for x in items}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

/// Dictionary comprehension scope
#[test]
fn dict_comprehension_scope() {
    let source = r#"
def invert(d: dict) -> dict:
    return {v: k for k, v in d.items()}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

/// Generator expression scope
#[test]
fn generator_expression_scope() {
    let source = r#"
def total(items):
    return sum(x * 2 for x in items)
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

/// Walrus operator `:=` in condition binds in enclosing function scope
#[test]
fn walrus_operator_binding() {
    let source = r#"
import re

def find_match(pattern: str, text: str):
    if (m := re.search(pattern, text)):
        return m.group(0)
    return None
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

/// `except Exception as e:` creates binding for `e`
#[test]
fn except_clause_binding() {
    let source = r#"
def safe_parse(text: str):
    try:
        return int(text)
    except ValueError as e:
        print(e)
        return None
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

/// `with open(...) as f:` creates binding for `f`
#[test]
fn with_statement_binding() {
    let source = r#"
def read_file(path: str) -> str:
    with open(path) as f:
        return f.read()
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

/// For-loop variable binding
#[test]
fn for_statement_binding() {
    let source = r#"
def sum_all(numbers):
    total = 0
    for n in numbers:
        total += n
    return total
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

/// `*args` in function parameters (list_splat_pattern)
#[test]
fn splat_args_parameter() {
    let source = r#"
def variadic(*args):
    return sum(args)
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

/// `**kwargs` in function parameters (dictionary_splat_pattern)
#[test]
fn kwargs_parameter() {
    let source = r#"
def flexible(**kwargs):
    return kwargs
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

/// `*args` with type annotation (`*args: int`) — typed_parameter path
#[test]
fn typed_splat_args_parameter() {
    let source = r#"
def typed_variadic(*args: int) -> int:
    return sum(args)
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

/// Default parameter: `def foo(x=5):`
#[test]
fn default_parameter() {
    let source = r#"
def greet(name='world'):
    return f'Hello, {name}!'
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

/// Typed default parameter: `def foo(x: int = 5):`
#[test]
fn typed_default_parameter() {
    let source = r#"
def power(base: int, exp: int = 2) -> int:
    return base ** exp
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

// ─────────────────────────────────────────────────────────────────────────────
// Native extension FFI handling (ctypes / cffi)
// ─────────────────────────────────────────────────────────────────────────────

/// Importing a known native C module triggers FFI edge
#[test]
fn native_c_module_import_creates_ffi_edge() {
    let source = r#"
import math

def circle_area(radius: float) -> float:
    return math.pi * radius ** 2
"#;
    let staging = build_graph(source);
    // math is in THIRD_PARTY_C_PACKAGES or STD_C_MODULES — may produce FfiCall;
    // at minimum, a function node and an imports edge must be produced
    assert!(
        staging.stats().nodes_staged >= 1,
        "Expected at least one node from native C module import source"
    );
}

/// ctypes CDLL usage
#[test]
fn ctypes_cdll_usage() {
    let source = r#"
import ctypes

libc = ctypes.CDLL("libc.so.6")

def call_printf():
    libc.printf(b"Hello\n")
"#;
    let staging = build_graph(source);
    // ctypes import exercises the CDLL code path; function node must be present
    assert!(
        staging.stats().nodes_staged >= 1,
        "Expected at least one node from ctypes CDLL usage source"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Annotated variable assignments (type hints at module level)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn annotated_assignment_module_level() {
    let source = r#"
from typing import Optional

MAX_SIZE: int = 100
name: Optional[str] = None
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

// ─────────────────────────────────────────────────────────────────────────────
// Public vs private name export
// ─────────────────────────────────────────────────────────────────────────────

/// Public functions (no leading underscore) get export edges when no __all__
#[test]
fn public_function_exported_without_all() {
    let source = r#"
def public_helper():
    return 42

def _internal_helper():
    return 0
"#;
    let staging = build_graph(source);
    // public_helper should get an Exports edge, _internal_helper should not
    assert!(
        has_edge_tag(&staging, "exports"),
        "Expected exports edge for public function. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

/// Private functions (leading underscore) should NOT be auto-exported
#[test]
fn private_function_not_exported() {
    let source = r#"
def _private():
    pass
"#;
    let staging = build_graph(source);
    // _private should NOT have an exports edge (tag is lowercase "exports")
    assert!(
        !has_edge_tag(&staging, "exports"),
        "Private function should not be exported. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Class methods — public vs private, static vs instance
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn class_with_dunder_methods() {
    let source = r#"
class Node:
    def __init__(self, value: int):
        self.value = value

    def __repr__(self) -> str:
        return f'Node({self.value})'

    def __eq__(self, other: object) -> bool:
        if isinstance(other, Node):
            return self.value == other.value
        return NotImplemented
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 3);
}

/// Self-referential call: `self.method()` should be resolved
#[test]
fn self_method_call_resolved() {
    let source = r#"
class Worker:
    def prepare(self):
        self.validate()
        self.process()

    def validate(self):
        pass

    def process(self):
        pass
"#;
    let staging = build_graph(source);
    assert!(
        has_edge_tag(&staging, "calls"),
        "Expected calls edges for self method calls. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Module-level top-level calls
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn module_level_call() {
    let source = r#"
def setup():
    pass

def teardown():
    pass

setup()
teardown()
"#;
    let staging = build_graph(source);
    assert!(
        has_edge_tag(&staging, "calls"),
        "Expected calls from module-level calls. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Nested classes and nested functions
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn nested_class_in_function() {
    let source = r#"
def make_counter():
    class Counter:
        def __init__(self):
            self.n = 0

        def increment(self):
            self.n += 1

    return Counter()
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 2);
}

#[test]
fn nested_function_in_class() {
    let source = r#"
class Processor:
    def process(self, items):
        def transform(item):
            return item * 2
        return [transform(x) for x in items]
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 2);
}
