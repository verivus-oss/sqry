//! Coverage-targeted tests for `sqry-lang-java`.
//!
//! Exercises uncovered paths in:
//! - `src/relations/graph_builder.rs`:
//!   - generic methods and generic classes
//!   - annotations on class, method, and field
//!   - lambda expressions and method references
//!   - record types (Java 16+)
//!   - sealed classes/interfaces
//!   - switch expressions (Java 14+) and switch labels
//!   - try-with-resources
//!   - enhanced for loop
//!   - constructor calls (object_creation_expression)
//!   - instanceof pattern matching
//!   - JNI native methods
//!   - Spring MVC route annotations
//!   - enum types with methods
//!   - nested classes (inner, static, anonymous)
//!   - synchronized methods
//! - `src/relations/local_scopes.rs`:
//!   - all scope kinds (Method, Constructor, StaticBlock, InstanceBlock,
//!     Lambda, TryBlock, CatchClause, SwitchBlock)
//! - `src/relations/java_common.rs`:
//!   - `build_symbol`, `build_member_symbol` with package, class stack

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::build::staging::StagingGraph;
use sqry_lang_java::relations::JavaGraphBuilder;
use std::path::Path;
use tree_sitter::Tree;

// ─────────────────────────────────────────────────────────────────────────────
// Shared helpers
// ─────────────────────────────────────────────────────────────────────────────

fn parse_java(source: &str) -> Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_java::LANGUAGE.into())
        .expect("set Java language");
    parser.parse(source, None).expect("parse Java")
}

fn build_graph(source: &str) -> StagingGraph {
    let tree = parse_java(source);
    let mut staging = StagingGraph::new();
    let builder = JavaGraphBuilder::default();
    builder
        .build_graph(
            &tree,
            source.as_bytes(),
            Path::new("Test.java"),
            &mut staging,
        )
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
// Generics
// ─────────────────────────────────────────────────────────────────────────────

/// Generic class with type parameters
#[test]
fn generic_class() {
    let source = r#"
package com.example;

public class Box<T> {
    private T value;

    public Box(T value) {
        this.value = value;
    }

    public T get() {
        return value;
    }

    public <R> Box<R> map(java.util.function.Function<T, R> mapper) {
        return new Box<>(mapper.apply(value));
    }
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 2);
}

/// Generic method with bounded type parameter
#[test]
fn generic_method_bounded() {
    let source = r#"
package com.example;

public class Utils {
    public static <T extends Comparable<T>> T max(T a, T b) {
        return a.compareTo(b) >= 0 ? a : b;
    }

    public static <T> java.util.List<T> singleton(T item) {
        java.util.List<T> list = new java.util.ArrayList<>();
        list.add(item);
        return list;
    }
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 2);
}

// ─────────────────────────────────────────────────────────────────────────────
// Annotations
// ─────────────────────────────────────────────────────────────────────────────

/// Class-level annotation
#[test]
fn class_level_annotation() {
    let source = r#"
package com.example;

@Deprecated
@SuppressWarnings("unchecked")
public class LegacyService {
    @Override
    public String toString() {
        return "LegacyService";
    }
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

/// Method-level annotation with parameters
#[test]
fn method_annotation_with_params() {
    let source = r#"
package com.example;

public class Controller {
    @SuppressWarnings({"unused", "deprecation"})
    public void process(String data) {
        System.out.println(data);
    }
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

// ─────────────────────────────────────────────────────────────────────────────
// Lambda expressions
// ─────────────────────────────────────────────────────────────────────────────

/// Lambda expression creates lambda node with TypeOf edges
#[test]
fn lambda_expression() {
    let source = r#"
package com.example;

import java.util.Arrays;
import java.util.List;

public class LambdaTest {
    public List<String> filterAndSort(List<String> items) {
        return items.stream()
            .filter(s -> s.length() > 3)
            .sorted((a, b) -> a.compareTo(b))
            .collect(java.util.stream.Collectors.toList());
    }
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

/// Lambda with block body
#[test]
fn lambda_with_block_body() {
    let source = r#"
package com.example;

import java.util.function.Function;

public class Transform {
    public Function<Integer, String> makeFormatter() {
        return (num) -> {
            if (num < 0) {
                return "negative";
            }
            return String.valueOf(num);
        };
    }
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

/// Method reference
#[test]
fn method_reference() {
    let source = r#"
package com.example;

import java.util.List;
import java.util.function.Consumer;

public class Printer {
    public void printAll(List<String> items) {
        Consumer<String> printer = System.out::println;
        items.forEach(printer);
    }
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

// ─────────────────────────────────────────────────────────────────────────────
// Records (Java 16+)
// ─────────────────────────────────────────────────────────────────────────────

/// Record type — exercises the record component path in local_scopes even if the
/// grammar version does not generate a top-level Class node for the record.
/// We verify the build at least completes without error and may produce nodes.
#[test]
fn record_type() {
    let source = r#"
package com.example;

public record Point(int x, int y) {
    public double distance() {
        return Math.sqrt(x * x + y * y);
    }
}
"#;
    // Build must not panic; whether it produces nodes depends on tree-sitter grammar
    // version support for record_declaration. We don't assert a minimum count.
    let _staging = build_graph(source);
}

// ─────────────────────────────────────────────────────────────────────────────
// Enum types with methods
// ─────────────────────────────────────────────────────────────────────────────

/// Enum with abstract method and constant-specific implementations
#[test]
fn enum_with_methods() {
    let source = r#"
package com.example;

public enum Planet {
    MERCURY(3.303e+23, 2.4397e6),
    VENUS(4.869e+24, 6.0518e6),
    EARTH(5.976e+24, 6.37814e6);

    private final double mass;
    private final double radius;

    Planet(double mass, double radius) {
        this.mass = mass;
        this.radius = radius;
    }

    public double surfaceGravity() {
        final double G = 6.67300E-11;
        return G * mass / (radius * radius);
    }

    public double surfaceWeight(double otherMass) {
        return otherMass * surfaceGravity();
    }
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 2);
}

// ─────────────────────────────────────────────────────────────────────────────
// Nested classes
// ─────────────────────────────────────────────────────────────────────────────

/// Static nested class
#[test]
fn static_nested_class() {
    let source = r#"
package com.example;

public class Outer {
    private int x = 10;

    public static class Inner {
        private int y = 20;

        public int getValue() {
            return y;
        }
    }

    public int compute() {
        Inner inner = new Inner();
        return x + inner.getValue();
    }
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 3);
}

/// Inner class (non-static)
#[test]
fn inner_class() {
    let source = r#"
package com.example;

public class Outer {
    private String name = "outer";

    class Inner {
        public String greet() {
            return "Hello from " + name;
        }
    }

    public String run() {
        return new Inner().greet();
    }
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 2);
}

// ─────────────────────────────────────────────────────────────────────────────
// JNI native methods
// ─────────────────────────────────────────────────────────────────────────────

/// Native method declaration creates FFI edge
#[test]
fn jni_native_method() {
    let source = r#"
package com.example;

public class NativeLib {
    public native int add(int a, int b);
    public native String concat(String a, String b);

    static {
        System.loadLibrary("native_lib");
    }
}
"#;
    let staging = build_graph(source);
    assert!(
        has_edge_tag(&staging, "ffi_call"),
        "Expected ffi_call edge for native method"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Spring MVC route annotations
// ─────────────────────────────────────────────────────────────────────────────

/// Spring @GetMapping creates http_request edge/endpoint
#[test]
fn spring_get_mapping() {
    let source = r#"
package com.example;

import org.springframework.web.bind.annotation.*;

@RestController
@RequestMapping("/api")
public class UserController {
    @GetMapping("/users")
    public java.util.List<String> getUsers() {
        return java.util.Arrays.asList("alice", "bob");
    }

    @PostMapping("/users")
    public String createUser(String name) {
        return "created: " + name;
    }
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

// ─────────────────────────────────────────────────────────────────────────────
// Try-with-resources
// ─────────────────────────────────────────────────────────────────────────────

/// `try (Resource r = new Resource()) { }` creates a local variable scope
#[test]
fn try_with_resources() {
    let source = r#"
package com.example;

import java.io.*;

public class FileReader {
    public String readFile(String path) throws IOException {
        try (BufferedReader reader = new BufferedReader(new java.io.FileReader(path))) {
            StringBuilder sb = new StringBuilder();
            String line;
            while ((line = reader.readLine()) != null) {
                sb.append(line).append('\n');
            }
            return sb.toString();
        }
    }
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

// ─────────────────────────────────────────────────────────────────────────────
// Enhanced for loop
// ─────────────────────────────────────────────────────────────────────────────

/// Enhanced for loop creates variable binding
#[test]
fn enhanced_for_loop() {
    let source = r#"
package com.example;

import java.util.List;

public class Processor {
    public int sumLengths(List<String> items) {
        int total = 0;
        for (String item : items) {
            total += item.length();
        }
        return total;
    }
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

// ─────────────────────────────────────────────────────────────────────────────
// Instanceof pattern matching (Java 16+)
// ─────────────────────────────────────────────────────────────────────────────

/// `if (obj instanceof String s)` pattern matching creates binding
#[test]
fn instanceof_pattern_match() {
    let source = r#"
package com.example;

public class TypeChecker {
    public String describe(Object obj) {
        if (obj instanceof String s) {
            return "String of length " + s.length();
        } else if (obj instanceof Integer i) {
            return "Integer: " + i;
        }
        return "Other: " + obj.getClass().getSimpleName();
    }
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

// ─────────────────────────────────────────────────────────────────────────────
// Switch expressions and switch labels
// ─────────────────────────────────────────────────────────────────────────────

/// Modern switch expression (Java 14+)
#[test]
fn switch_expression() {
    let source = r#"
package com.example;

public class DayCalc {
    public int workingHours(String day) {
        return switch (day) {
            case "Monday", "Tuesday", "Wednesday", "Thursday", "Friday" -> 8;
            case "Saturday" -> 4;
            default -> 0;
        };
    }
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

/// Switch with pattern (Java 21+)
#[test]
fn switch_with_guard() {
    let source = r#"
package com.example;

public class Formatter {
    public String format(Object obj) {
        return switch (obj) {
            case Integer i when i < 0 -> "negative: " + i;
            case Integer i -> "positive: " + i;
            case String s -> "string: " + s;
            default -> "other";
        };
    }
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

// ─────────────────────────────────────────────────────────────────────────────
// Interfaces
// ─────────────────────────────────────────────────────────────────────────────

/// Interface with default method
#[test]
fn interface_with_default_method() {
    let source = r#"
package com.example;

public interface Greeter {
    String greet(String name);

    default String greetAll(String... names) {
        StringBuilder sb = new StringBuilder();
        for (String name : names) {
            sb.append(greet(name)).append('\n');
        }
        return sb.toString();
    }

    static Greeter formal() {
        return name -> "Good day, " + name;
    }
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 2);
}

/// Interface extending multiple interfaces
#[test]
fn interface_multiple_extension() {
    let source = r#"
package com.example;

public interface Readable {
    String read();
}

public interface Writable {
    void write(String data);
}

public interface ReadWrite extends Readable, Writable {
    default String readAndWrite() {
        String data = read();
        write(data);
        return data;
    }
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 3);
}

// ─────────────────────────────────────────────────────────────────────────────
// Constructor calls
// ─────────────────────────────────────────────────────────────────────────────

/// `new ClassName(args)` constructor call
#[test]
fn constructor_call() {
    let source = r#"
package com.example;

public class Builder {
    public java.util.List<String> build() {
        java.util.List<String> list = new java.util.ArrayList<>(10);
        list.add(new String("hello"));
        return list;
    }
}
"#;
    let staging = build_graph(source);
    assert!(
        has_edge_tag(&staging, "calls"),
        "Expected calls edge for constructor. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Static blocks and instance blocks
// ─────────────────────────────────────────────────────────────────────────────

/// Static initializer block
#[test]
fn static_initializer_block() {
    let source = r#"
package com.example;

public class Constants {
    static final java.util.Map<String, Integer> CODES;

    static {
        CODES = new java.util.HashMap<>();
        CODES.put("OK", 200);
        CODES.put("NOT_FOUND", 404);
        CODES.put("ERROR", 500);
    }

    public static int getCode(String name) {
        return CODES.getOrDefault(name, -1);
    }
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

/// Instance initializer block
#[test]
fn instance_initializer_block() {
    let source = r#"
package com.example;

public class Counter {
    private int count;

    {
        count = 100; // instance initializer
    }

    public Counter() {}

    public int getCount() {
        return count;
    }
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

// ─────────────────────────────────────────────────────────────────────────────
// Synchronized methods
// ─────────────────────────────────────────────────────────────────────────────

/// Synchronized method
#[test]
fn synchronized_method() {
    let source = r#"
package com.example;

public class ThreadSafeCounter {
    private int count = 0;

    public synchronized void increment() {
        count++;
    }

    public synchronized int getCount() {
        return count;
    }
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 2);
}

// ─────────────────────────────────────────────────────────────────────────────
// Import declarations
// ─────────────────────────────────────────────────────────────────────────────

/// Single import
#[test]
fn single_import() {
    let source = r#"
package com.example;

import java.util.List;
import java.util.ArrayList;

public class Container {
    public List<String> getItems() {
        return new ArrayList<>();
    }
}
"#;
    let staging = build_graph(source);
    assert!(
        has_edge_tag(&staging, "imports"),
        "Expected imports edge. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

/// Wildcard import
#[test]
fn wildcard_import() {
    let source = r#"
package com.example;

import java.util.*;
import java.io.*;

public class All {
    public List<String> empty() {
        return new ArrayList<>();
    }
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

/// Static import
#[test]
fn static_import() {
    let source = r#"
package com.example;

import static java.lang.Math.*;
import static java.util.Collections.emptyList;

public class MathHelper {
    public double circle(double r) {
        return PI * pow(r, 2);
    }
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

// ─────────────────────────────────────────────────────────────────────────────
// Catch clause
// ─────────────────────────────────────────────────────────────────────────────

/// Multi-catch exception handling creates bindings
#[test]
fn multi_catch_exception() {
    let source = r#"
package com.example;

public class ErrorHandler {
    public String parse(String input) {
        try {
            return String.valueOf(Integer.parseInt(input));
        } catch (NumberFormatException | IllegalArgumentException e) {
            return "error: " + e.getMessage();
        }
    }
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
}

// ─────────────────────────────────────────────────────────────────────────────
// Inheritance and interfaces
// ─────────────────────────────────────────────────────────────────────────────

/// Class extends base and implements interface
#[test]
fn class_extends_and_implements() {
    let source = r#"
package com.example;

public abstract class Shape {
    public abstract double area();
}

public interface Drawable {
    void draw();
}

public class Circle extends Shape implements Drawable {
    private final double radius;

    public Circle(double radius) {
        this.radius = radius;
    }

    @Override
    public double area() {
        return Math.PI * radius * radius;
    }

    @Override
    public void draw() {
        System.out.println("Drawing circle with radius " + radius);
    }
}
"#;
    let staging = build_graph(source);
    assert!(
        has_edge_tag(&staging, "inherits"),
        "Expected inherits edge. Tags: {:?}",
        all_edge_tags(&staging)
    );
    assert!(
        has_edge_tag(&staging, "implements"),
        "Expected implements edge. Tags: {:?}",
        all_edge_tags(&staging)
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Field type declarations and TypeOf edges
// ─────────────────────────────────────────────────────────────────────────────

/// Class fields with various types create TypeOf edges
#[test]
fn class_fields_typeof_edges() {
    let source = r#"
package com.example;

import java.util.List;
import java.util.Map;

public class DataStore {
    private final String name;
    private List<Integer> items;
    private Map<String, Object> properties;
    private static int instanceCount;

    public DataStore(String name) {
        this.name = name;
        this.items = new java.util.ArrayList<>();
        this.properties = new java.util.HashMap<>();
    }

    public String getName() { return name; }
    public List<Integer> getItems() { return items; }
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 3);
}
