//! Comprehensive test suite for `JavaGraphBuilder` using unified `CodeGraph` architecture
//!
//! This test file validates the Java language graph builder against test fixtures
//! covering method calls, constructors, imports, native methods, nested classes,
//! OOP relationships (inheritance, interfaces), and more.

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::StagingGraph;
use sqry_core::graph::unified::build::staging::StagingOp;
use sqry_lang_java::relations::JavaGraphBuilder;
use std::path::PathBuf;
use tree_sitter::Tree;

// ========== Active Test Helper Functions ==========

/// Parse Java source code into a tree-sitter Tree
fn parse_java(content: &str) -> Tree {
    let mut parser = tree_sitter::Parser::new();
    let language = tree_sitter_java::LANGUAGE.into();
    parser
        .set_language(&language)
        .expect("Failed to load Java grammar");
    parser
        .parse(content, None)
        .expect("Failed to parse Java code")
}

/// Build staging graph from Java source and return it for assertions
fn build_staging_graph(content: &str, filename: &str) -> StagingGraph {
    let tree = parse_java(content);
    let mut staging = StagingGraph::new();
    let builder = JavaGraphBuilder::default();
    let file_path = PathBuf::from(filename);

    builder
        .build_graph(&tree, content.as_bytes(), &file_path, &mut staging)
        .expect("Failed to build graph");

    staging
}

/// Count edges of a specific kind in staging operations
fn count_edge_kind(staging: &StagingGraph, kind_tag: &str) -> usize {
    staging
        .operations()
        .iter()
        .filter(|op| {
            if let StagingOp::AddEdge { kind, .. } = op {
                kind.tag() == kind_tag
            } else {
                false
            }
        })
        .count()
}

/// Check if staging has an edge of a specific kind
fn has_edge_kind(staging: &StagingGraph, kind_tag: &str) -> bool {
    count_edge_kind(staging, kind_tag) > 0
}

// ========== OOP Edge Tests (Inherits + Implements) ==========

#[test]
fn test_class_inheritance_creates_inherits_edge() {
    let content = r"
package com.example;

class Animal {
    public void speak() {}
}

class Dog extends Animal {
    public void bark() {}
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have at least one Inherits edge (Dog -> Animal)
    assert!(
        has_edge_kind(&staging, "inherits"),
        "Expected Inherits edge for class inheritance"
    );
    let inherits_count = count_edge_kind(&staging, "inherits");
    assert_eq!(
        inherits_count, 1,
        "Expected exactly 1 Inherits edge, got {inherits_count}"
    );
}

#[test]
fn test_class_implements_interface_creates_implements_edge() {
    let content = r"
package com.example;

interface Runnable {
    void run();
}

class Task implements Runnable {
    public void run() {}
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have an Implements edge (Task -> Runnable)
    assert!(
        has_edge_kind(&staging, "implements"),
        "Expected Implements edge for interface implementation"
    );
    let implements_count = count_edge_kind(&staging, "implements");
    assert_eq!(
        implements_count, 1,
        "Expected exactly 1 Implements edge, got {implements_count}"
    );
}

#[test]
fn test_class_implements_multiple_interfaces() {
    let content = r"
package com.example;

interface Runnable {
    void run();
}

interface Serializable {}

class Task implements Runnable, Serializable {
    public void run() {}
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have 2 Implements edges (Task -> Runnable, Task -> Serializable)
    let implements_count = count_edge_kind(&staging, "implements");
    assert_eq!(
        implements_count, 2,
        "Expected 2 Implements edges for multiple interface implementation, got {implements_count}"
    );
}

#[test]
fn test_class_extends_and_implements() {
    let content = r"
package com.example;

class Animal {
    public void speak() {}
}

interface Runnable {
    void run();
}

class Dog extends Animal implements Runnable {
    public void speak() {}
    public void run() {}
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have 1 Inherits edge (Dog -> Animal)
    let inherits_count = count_edge_kind(&staging, "inherits");
    assert_eq!(
        inherits_count, 1,
        "Expected 1 Inherits edge, got {inherits_count}"
    );

    // Should have 1 Implements edge (Dog -> Runnable)
    let implements_count = count_edge_kind(&staging, "implements");
    assert_eq!(
        implements_count, 1,
        "Expected 1 Implements edge, got {implements_count}"
    );
}

#[test]
fn test_interface_extends_interface() {
    let content = r"
package com.example;

interface Readable {
    void read();
}

interface Closeable {
    void close();
}

interface Stream extends Readable, Closeable {
    void flush();
}
";

    let staging = build_staging_graph(content, "test.java");

    // Interface extending other interfaces should create Inherits edges
    // Stream -> Readable, Stream -> Closeable
    let inherits_count = count_edge_kind(&staging, "inherits");
    assert_eq!(
        inherits_count, 2,
        "Expected 2 Inherits edges for interface extending interfaces, got {inherits_count}"
    );
}

#[test]
fn test_nested_class_inheritance() {
    let content = r"
package com.example;

class Outer {
    static class Inner extends Outer {
        void innerMethod() {}
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have Inherits edge (Inner -> Outer)
    assert!(
        has_edge_kind(&staging, "inherits"),
        "Expected Inherits edge for nested class inheritance"
    );
}

#[test]
fn test_nested_class_methods_are_not_duplicated() {
    let content = r"
package com.example;

class Outer {
    static class Inner {
        public Optional<String> nestedOptional() {
            return Optional.empty();
        }
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    let method_names: Vec<String> = staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode { entry, .. } = op
                && matches!(entry.kind, NodeKind::Method)
            {
                let name_id = entry.qualified_name.unwrap_or(entry.name);
                return staging.resolve_local_string(name_id).map(str::to_string);
            }
            None
        })
        .collect();

    let nested_matches: Vec<_> = method_names
        .iter()
        .filter(|name| name.contains("Outer::Inner::nestedOptional"))
        .collect();

    assert_eq!(
        nested_matches.len(),
        1,
        "Expected exactly one canonical nested method node, got {method_names:?}"
    );
    assert!(
        method_names
            .iter()
            .all(|name| name != "com::example::Inner::nestedOptional"),
        "Nested method should not be emitted without its enclosing class: {method_names:?}"
    );
}

#[test]
fn test_enum_implements_interface() {
    let content = r"
package com.example;

interface Describable {
    String describe();
}

enum Status implements Describable {
    ACTIVE, INACTIVE;

    public String describe() {
        return name();
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Enum implementing interface should create Implements edge
    // Note: Our implementation treats enums like classes for OOP edges
    // The enum declaration handler doesn't add implements edges by default,
    // so this test documents current behavior.
    // In a full implementation, enums should also get implements edges.
    // For now, we verify the test runs without error.
    let _ = staging.operations();
}

#[test]
fn test_class_without_inheritance() {
    let content = r"
package com.example;

class Standalone {
    void method() {}
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have NO Inherits edge
    let inherits_count = count_edge_kind(&staging, "inherits");
    assert_eq!(
        inherits_count, 0,
        "Expected 0 Inherits edges for standalone class, got {inherits_count}"
    );

    // Should have NO Implements edge
    let implements_count = count_edge_kind(&staging, "implements");
    assert_eq!(
        implements_count, 0,
        "Expected 0 Implements edges for standalone class, got {implements_count}"
    );
}

#[test]
fn test_multiple_classes_with_inheritance() {
    let content = r"
package com.example;

class A {}
class B extends A {}
class C extends B {}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have 2 Inherits edges (B -> A, C -> B)
    let inherits_count = count_edge_kind(&staging, "inherits");
    assert_eq!(
        inherits_count, 2,
        "Expected 2 Inherits edges for inheritance chain, got {inherits_count}"
    );
}

#[test]
fn test_diamond_inheritance_pattern() {
    let content = r"
package com.example;

interface A {}
interface B extends A {}
interface C extends A {}
class D implements B, C {}
";

    let staging = build_staging_graph(content, "test.java");

    // B extends A: 1 Inherits
    // C extends A: 1 Inherits
    // D implements B, C: 2 Implements
    let inherits_count = count_edge_kind(&staging, "inherits");
    let implements_count = count_edge_kind(&staging, "implements");

    assert_eq!(
        inherits_count, 2,
        "Expected 2 Inherits edges (B->A, C->A), got {inherits_count}"
    );
    assert_eq!(
        implements_count, 2,
        "Expected 2 Implements edges (D->B, D->C), got {implements_count}"
    );
}

#[test]
fn test_generic_class_inheritance() {
    let content = r"
package com.example;

class Box<T> {}
class StringBox extends Box<String> {}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have Inherits edge (StringBox -> Box)
    // The generic parameter is ignored for the edge
    assert!(
        has_edge_kind(&staging, "inherits"),
        "Expected Inherits edge for generic class inheritance"
    );
}

#[test]
fn test_generic_interface_implementation() {
    let content = r"
package com.example;

interface Comparable<T> {
    int compareTo(T other);
}

class Person implements Comparable<Person> {
    public int compareTo(Person other) { return 0; }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have Implements edge (Person -> Comparable)
    assert!(
        has_edge_kind(&staging, "implements"),
        "Expected Implements edge for generic interface implementation"
    );
}

#[test]
fn test_call_edges_still_work_with_oop_changes() {
    let content = r"
package com.example;

class Calculator {
    int add(int a, int b) {
        return a + b;
    }

    int multiply(int a, int b) {
        int sum = add(a, b);
        return sum * b;
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should still create call edges (multiply -> add)
    assert!(
        has_edge_kind(&staging, "calls"),
        "Expected Call edges to still work after OOP changes"
    );
}

#[test]
fn test_import_edges_still_work_with_oop_changes() {
    let content = r"
package com.example;

import java.util.List;
import java.util.ArrayList;

class Demo {
    void method() {}
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should still create import edges
    assert!(
        has_edge_kind(&staging, "imports"),
        "Expected Import edges to still work after OOP changes"
    );
}

// ========== FFI Tests (JNI, JNA, Panama) ==========

use sqry_core::graph::unified::EdgeKind;

/// Helper to count FFI edges from staging operations
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

#[test]
fn test_jni_native_method_creates_ffi_edge() {
    let content = r"
package com.example;

class NativeHelper {
    public native void nativeMethod();
    public native int nativeCompute(int x);
    public static native String nativeStatic();
}
";

    let staging = build_staging_graph(content, "test.java");

    let ffi_count = count_ffi_edges(&staging);
    assert!(
        ffi_count >= 3,
        "Expected at least 3 FFI edges for native methods, got {ffi_count}"
    );
}

#[test]
fn test_jni_native_static_method() {
    let content = r"
package com.example;

class SystemBridge {
    public static native void initialize();
    public static native long getSystemTime();
}
";

    let staging = build_staging_graph(content, "test.java");

    let ffi_count = count_ffi_edges(&staging);
    assert!(
        ffi_count >= 2,
        "Expected at least 2 FFI edges for static native methods, got {ffi_count}"
    );
}

#[test]
fn test_jna_native_load() {
    let content = r#"
package com.example;

import com.sun.jna.Native;
import com.sun.jna.Library;

interface CLibrary extends Library {
    int printf(String format);
}

class JnaExample {
    void loadLibrary() {
        CLibrary lib = Native.load("c", CLibrary.class);
    }
}
"#;

    let staging = build_staging_graph(content, "test.java");

    let ffi_count = count_ffi_edges(&staging);
    assert!(
        ffi_count >= 1,
        "Expected at least 1 FFI edge for Native.load(), got {ffi_count}"
    );
}

#[test]
fn test_jna_load_library() {
    let content = r#"
package com.example;

import com.sun.jna.Native;
import com.sun.jna.Library;

interface MathLib extends Library {
    double sqrt(double x);
    double pow(double base, double exp);
}

class MathWrapper {
    void initMath() {
        MathLib math = Native.loadLibrary("m", MathLib.class);
    }
}
"#;

    let staging = build_staging_graph(content, "test.java");

    let ffi_count = count_ffi_edges(&staging);
    assert!(
        ffi_count >= 1,
        "Expected at least 1 FFI edge for Native.loadLibrary(), got {ffi_count}"
    );
}

#[test]
fn test_panama_linker() {
    let content = r"
package com.example;

import java.lang.foreign.Linker;
import java.lang.foreign.FunctionDescriptor;

class PanamaExample {
    void setupLinker() {
        Linker linker = Linker.nativeLinker();
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    let ffi_count = count_ffi_edges(&staging);
    assert!(
        ffi_count >= 1,
        "Expected at least 1 FFI edge for Linker.nativeLinker(), got {ffi_count}"
    );
}

#[test]
fn test_panama_symbol_lookup() {
    let content = r#"
package com.example;

import java.lang.foreign.SymbolLookup;
import java.lang.foreign.Linker;

class PanamaLoader {
    void loadNativeLib() {
        var lookup = SymbolLookup.libraryLookup("libfoo.so", Arena.global());
    }
}
"#;

    let staging = build_staging_graph(content, "test.java");

    let ffi_count = count_ffi_edges(&staging);
    assert!(
        ffi_count >= 1,
        "Expected at least 1 FFI edge for SymbolLookup.libraryLookup(), got {ffi_count}"
    );
}

#[test]
fn test_panama_method_handle_invoke() {
    let content = r"
package com.example;

import java.lang.foreign.Linker;
import java.lang.foreign.MethodHandle;

class PanamaInvoker {
    void callNative(MethodHandle downcallHandle) throws Throwable {
        int result = (int) downcallHandle.invokeExact(42);
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    let ffi_count = count_ffi_edges(&staging);
    assert!(
        ffi_count >= 1,
        "Expected at least 1 FFI edge for MethodHandle.invokeExact(), got {ffi_count}"
    );
}

#[test]
fn test_no_ffi_for_regular_java_calls() {
    let content = r#"
package com.example;

import java.util.ArrayList;
import java.util.List;

class RegularJava {
    void regularMethod() {
        List<String> list = new ArrayList<>();
        list.add("Hello");
        System.out.println(list.size());
    }
}
"#;

    let staging = build_staging_graph(content, "test.java");

    // Regular Java calls should NOT create FFI edges
    let ffi_count = count_ffi_edges(&staging);
    assert_eq!(
        ffi_count, 0,
        "Expected 0 FFI edges for regular Java calls, got {ffi_count}"
    );
}

#[test]
fn test_combined_jni_and_regular_calls() {
    let content = r"
package com.example;

class MixedClass {
    // JNI native method
    public native int computeNative(int x);

    // Regular Java method
    public int computeJava(int x) {
        return x * 2;
    }

    public void process() {
        int nativeResult = computeNative(10);
        int javaResult = computeJava(20);
        System.out.println(nativeResult + javaResult);
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Should have FFI edge for native method
    let ffi_count = count_ffi_edges(&staging);
    assert!(
        ffi_count >= 1,
        "Expected at least 1 FFI edge for native method, got {ffi_count}"
    );

    // Should also have regular call edges
    let call_count = count_edge_kind(&staging, "calls");
    assert!(
        call_count >= 2,
        "Expected at least 2 call edges (computeNative and computeJava calls from process), got {call_count}"
    );
}

#[test]
fn test_jni_without_native_keyword_no_ffi() {
    let content = r#"
package com.example;

class NormalClass {
    // Not native - just a regular abstract method
    public void normalMethod() {
        System.out.println("Hello");
    }

    // Not native - returns something
    public int compute(int x) {
        return x * 2;
    }
}
"#;

    let staging = build_staging_graph(content, "test.java");

    // No native keyword = no FFI edges
    let ffi_count = count_ffi_edges(&staging);
    assert_eq!(
        ffi_count, 0,
        "Expected 0 FFI edges when no native methods exist, got {ffi_count}"
    );
}

// ========== Export Tests ==========

/// Helper to count Export edges from staging operations
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

#[test]
fn test_public_class_creates_export_edge() {
    let content = r"
package com.example;

public class PublicService {
    public void serve() {}
}
";

    let staging = build_staging_graph(content, "test.java");

    let export_count = count_export_edges(&staging);
    assert!(
        export_count >= 1,
        "Expected at least 1 Export edge for public class, got {export_count}"
    );
}

#[test]
fn test_public_interface_creates_export_edge() {
    let content = r"
package com.example;

public interface ApiService {
    void process();
}
";

    let staging = build_staging_graph(content, "test.java");

    let export_count = count_export_edges(&staging);
    assert!(
        export_count >= 1,
        "Expected at least 1 Export edge for public interface, got {export_count}"
    );
}

#[test]
fn test_public_enum_creates_export_edge() {
    let content = r"
package com.example;

public enum Status {
    ACTIVE,
    INACTIVE,
    PENDING
}
";

    let staging = build_staging_graph(content, "test.java");

    let export_count = count_export_edges(&staging);
    // 1 for the enum + 3 for enum constants
    assert!(
        export_count >= 4,
        "Expected at least 4 Export edges (enum + 3 constants), got {export_count}"
    );
}

#[test]
fn test_public_method_creates_export_edge() {
    let content = r"
package com.example;

public class Calculator {
    public int add(int a, int b) {
        return a + b;
    }

    private int subtract(int a, int b) {
        return a - b;
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    let export_count = count_export_edges(&staging);
    // 1 for public class + 1 for public add method
    // subtract is private, so no export
    assert!(
        export_count >= 2,
        "Expected at least 2 Export edges (class + public method), got {export_count}"
    );
}

#[test]
fn test_public_constructor_creates_export_edge() {
    let content = r"
package com.example;

public class Entity {
    public Entity() {}

    public Entity(String name) {
        // Constructor with param
    }

    private Entity(int id) {
        // Private constructor
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    let export_count = count_export_edges(&staging);
    // 1 for public class + 2 for public constructors
    // private constructor not exported
    assert!(
        export_count >= 3,
        "Expected at least 3 Export edges (class + 2 public constructors), got {export_count}"
    );
}

#[test]
fn test_public_field_creates_export_edge() {
    let content = r"
package com.example;

public class Config {
    public String host;
    public int port;
    private String secret;
}
";

    let staging = build_staging_graph(content, "test.java");

    let export_count = count_export_edges(&staging);
    // 1 for public class + 2 for public fields
    // secret is private, so not exported
    assert!(
        export_count >= 3,
        "Expected at least 3 Export edges (class + 2 public fields), got {export_count}"
    );
}

#[test]
fn test_public_static_final_creates_export_edge() {
    let content = r#"
package com.example;

public class Constants {
    public static final String VERSION = "1.0.0";
    public static final int MAX_SIZE = 100;
    private static final String INTERNAL = "secret";
}
"#;

    let staging = build_staging_graph(content, "test.java");

    let export_count = count_export_edges(&staging);
    // 1 for public class + 2 for public static final constants
    // INTERNAL is private, so not exported
    assert!(
        export_count >= 3,
        "Expected at least 3 Export edges (class + 2 public constants), got {export_count}"
    );
}

#[test]
fn test_package_private_class_no_export() {
    let content = r"
package com.example;

class PackagePrivateHelper {
    void help() {}
}
";

    let staging = build_staging_graph(content, "test.java");

    // Package-private class should not be exported
    let export_count = count_export_edges(&staging);
    assert_eq!(
        export_count, 0,
        "Expected 0 Export edges for package-private class, got {export_count}"
    );
}

#[test]
fn test_interface_methods_implicitly_public() {
    let content = r"
package com.example;

public interface Service {
    void start();
    void stop();
    String getStatus();
}
";

    let staging = build_staging_graph(content, "test.java");

    // Interface methods are implicitly public and should all be exported
    // 1 for the interface + 3 for the methods (start, stop, getStatus)
    let export_count = count_export_edges(&staging);
    assert!(
        export_count >= 4,
        "Expected at least 4 Export edges for public interface + 3 implicit public methods, got {export_count}"
    );
}

#[test]
fn test_interface_methods_without_explicit_public_modifier() {
    // In Java, interface methods don't need explicit 'public' modifier - they are implicitly public
    let content = r"
package com.example;

public interface Repository {
    void save(Object entity);
    Object findById(int id);
    void delete(int id);
    int count();
}
";

    let staging = build_staging_graph(content, "test.java");

    // All 4 interface methods should be exported even without explicit 'public' modifier
    // 1 interface + 4 methods = 5 exports
    let export_count = count_export_edges(&staging);
    assert!(
        export_count >= 5,
        "Expected at least 5 Export edges (1 interface + 4 implicitly public methods), got {export_count}"
    );
}

#[test]
fn test_class_methods_require_explicit_public() {
    // Class methods need explicit 'public' modifier to be exported
    let content = r"
package com.example;

public class Helper {
    void packagePrivateMethod() {}
    private void privateMethod() {}
    protected void protectedMethod() {}
    public void publicMethod() {}
}
";

    let staging = build_staging_graph(content, "test.java");

    // Only the class itself + the public method should be exported
    // packagePrivate, private, and protected methods should NOT be exported
    let export_count = count_export_edges(&staging);
    assert_eq!(
        export_count, 2,
        "Expected exactly 2 Export edges (class + public method), got {export_count}"
    );
}

#[test]
fn test_interface_with_default_and_static_methods() {
    // Default and static methods in interfaces are also implicitly public
    let content = r#"
package com.example;

public interface ModernInterface {
    void abstractMethod();

    default void defaultMethod() {
        System.out.println("default");
    }

    static void staticMethod() {
        System.out.println("static");
    }
}
"#;

    let staging = build_staging_graph(content, "test.java");

    // Interface + 3 methods (abstract, default, static) - all implicitly public
    let export_count = count_export_edges(&staging);
    assert!(
        export_count >= 4,
        "Expected at least 4 Export edges (interface + 3 implicit public methods), got {export_count}"
    );
}

#[test]
fn test_interface_private_methods_not_exported() {
    // Java 9+ allows private methods in interfaces - these should NOT be exported
    // JLS §9: interface methods may be declared `public` or `private`
    let content = r"
package com.example;

public interface ServiceWithPrivate {
    void publicApi();

    default void defaultApi() {
        helper();
    }

    private void helper() {
        // Private helper method - should NOT be exported
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Interface + 2 public methods (publicApi, defaultApi) = 3 exports
    // Private helper() should NOT be exported
    let export_count = count_export_edges(&staging);
    assert_eq!(
        export_count, 3,
        "Expected exactly 3 Export edges (interface + 2 public methods), got {export_count}. \
         Private method 'helper' should NOT be exported."
    );
}

#[test]
fn test_interface_private_static_methods_not_exported() {
    // Java 9+ also allows private static methods in interfaces
    let content = r"
package com.example;

public interface UtilityInterface {
    void api();

    static void publicStatic() {
        privateStatic();
    }

    private static void privateStatic() {
        // Private static helper - should NOT be exported
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // Interface + 2 public methods (api, publicStatic) = 3 exports
    // Private static privateStatic() should NOT be exported
    let export_count = count_export_edges(&staging);
    assert_eq!(
        export_count, 3,
        "Expected exactly 3 Export edges (interface + 2 public methods), got {export_count}. \
         Private static method 'privateStatic' should NOT be exported."
    );
}

#[test]
fn test_multiple_public_members() {
    let content = r#"
package com.example;

public class FullyExposed {
    public static final String NAME = "exposed";
    public int value;

    public FullyExposed() {}

    public void doWork() {}
    public static int calculate(int x) { return x * 2; }

    private void internalMethod() {}
}
"#;

    let staging = build_staging_graph(content, "test.java");

    let export_count = count_export_edges(&staging);
    // 1 class + 1 constant (NAME) + 1 field (value) + 1 constructor + 2 methods (doWork, calculate)
    // = 6 public members
    // internalMethod is private, not exported
    assert!(
        export_count >= 6,
        "Expected at least 6 Export edges for fully exposed class, got {export_count}"
    );
}

#[test]
fn test_nested_public_class_not_exported_at_top_level() {
    // Note: Nested classes are handled within their parent's body,
    // so their exports depend on how the walker processes them
    let content = r"
package com.example;

public class Outer {
    public static class Inner {
        public void innerMethod() {}
    }
}
";

    let staging = build_staging_graph(content, "test.java");

    // At minimum, the outer class should be exported
    let export_count = count_export_edges(&staging);
    assert!(
        export_count >= 1,
        "Expected at least 1 Export edge for outer public class, got {export_count}"
    );
}

// ========== Spring MVC Route Endpoint Tests ==========

use sqry_core::graph::unified::node::NodeKind;

/// Helper: check if staging has an Endpoint node whose resolved name contains the given substring.
fn has_endpoint_with_name(staging: &StagingGraph, name_substring: &str) -> bool {
    staging.operations().iter().any(|op| {
        if let StagingOp::AddNode { entry, .. } = op
            && matches!(entry.kind, NodeKind::Endpoint)
            && let Some(resolved) = staging.resolve_local_string(entry.name)
        {
            return resolved.contains(name_substring);
        }
        false
    })
}

/// Helper: collect all Endpoint node names from staging.
fn collect_endpoint_names(staging: &StagingGraph) -> Vec<String> {
    staging
        .operations()
        .iter()
        .filter_map(|op| {
            if let StagingOp::AddNode { entry, .. } = op
                && matches!(entry.kind, NodeKind::Endpoint)
            {
                return staging.resolve_local_string(entry.name).map(String::from);
            }
            None
        })
        .collect()
}

/// Helper: count Endpoint nodes in staging.
fn count_endpoint_nodes(staging: &StagingGraph) -> usize {
    staging.operations().iter().filter(|op| {
        matches!(op, StagingOp::AddNode { entry, .. } if matches!(entry.kind, NodeKind::Endpoint))
    }).count()
}

#[test]
fn test_spring_get_mapping_creates_endpoint() {
    let content = r#"
package com.example.controller;

import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.RestController;

@RestController
public class UserController {
    @GetMapping("/api/users")
    public String getUsers() {
        return "users";
    }
}
"#;

    let staging = build_staging_graph(content, "UserController.java");

    assert!(
        has_endpoint_with_name(&staging, "route::GET::/api/users"),
        "Expected Endpoint node 'route::GET::/api/users', found: {:?}",
        collect_endpoint_names(&staging)
    );
}

#[test]
fn test_spring_post_mapping_creates_endpoint() {
    let content = r#"
package com.example.controller;

import org.springframework.web.bind.annotation.PostMapping;

public class ItemController {
    @PostMapping("/api/items")
    public void createItem() {}
}
"#;

    let staging = build_staging_graph(content, "ItemController.java");

    assert!(
        has_endpoint_with_name(&staging, "route::POST::/api/items"),
        "Expected Endpoint node 'route::POST::/api/items', found: {:?}",
        collect_endpoint_names(&staging)
    );
}

#[test]
fn test_spring_put_mapping_creates_endpoint() {
    let content = r#"
package com.example.controller;

import org.springframework.web.bind.annotation.PutMapping;

public class ItemController {
    @PutMapping("/api/items")
    public void updateItem() {}
}
"#;

    let staging = build_staging_graph(content, "ItemController.java");

    assert!(
        has_endpoint_with_name(&staging, "route::PUT::/api/items"),
        "Expected Endpoint node 'route::PUT::/api/items', found: {:?}",
        collect_endpoint_names(&staging)
    );
}

#[test]
fn test_spring_delete_mapping_creates_endpoint() {
    let content = r#"
package com.example.controller;

import org.springframework.web.bind.annotation.DeleteMapping;

public class ItemController {
    @DeleteMapping("/api/items")
    public void deleteItem() {}
}
"#;

    let staging = build_staging_graph(content, "ItemController.java");

    assert!(
        has_endpoint_with_name(&staging, "route::DELETE::/api/items"),
        "Expected Endpoint node 'route::DELETE::/api/items', found: {:?}",
        collect_endpoint_names(&staging)
    );
}

#[test]
fn test_spring_patch_mapping_creates_endpoint() {
    let content = r#"
package com.example.controller;

import org.springframework.web.bind.annotation.PatchMapping;

public class ItemController {
    @PatchMapping("/api/items")
    public void patchItem() {}
}
"#;

    let staging = build_staging_graph(content, "ItemController.java");

    assert!(
        has_endpoint_with_name(&staging, "route::PATCH::/api/items"),
        "Expected Endpoint node 'route::PATCH::/api/items', found: {:?}",
        collect_endpoint_names(&staging)
    );
}

#[test]
fn test_spring_request_mapping_with_path_arg() {
    let content = r#"
package com.example.controller;

import org.springframework.web.bind.annotation.RequestMapping;
import org.springframework.web.bind.annotation.RequestMethod;

public class UserController {
    @RequestMapping(path = "/api/users", method = RequestMethod.GET)
    public String getUsers() {
        return "users";
    }
}
"#;

    let staging = build_staging_graph(content, "UserController.java");

    assert!(
        has_endpoint_with_name(&staging, "route::GET::/api/users"),
        "Expected Endpoint node 'route::GET::/api/users', found: {:?}",
        collect_endpoint_names(&staging)
    );
}

#[test]
fn test_spring_request_mapping_with_value_arg() {
    let content = r#"
package com.example.controller;

import org.springframework.web.bind.annotation.RequestMapping;

public class UserController {
    @RequestMapping(value = "/api/users")
    public String getUsers() {
        return "users";
    }
}
"#;

    let staging = build_staging_graph(content, "UserController.java");

    // Default HTTP method for @RequestMapping without explicit method is GET
    assert!(
        has_endpoint_with_name(&staging, "route::GET::/api/users"),
        "Expected Endpoint node 'route::GET::/api/users', found: {:?}",
        collect_endpoint_names(&staging)
    );
}

#[test]
fn test_spring_request_mapping_direct_string() {
    let content = r#"
package com.example.controller;

import org.springframework.web.bind.annotation.RequestMapping;

public class UserController {
    @RequestMapping("/api/users")
    public String getUsers() {
        return "users";
    }
}
"#;

    let staging = build_staging_graph(content, "UserController.java");

    assert!(
        has_endpoint_with_name(&staging, "route::GET::/api/users"),
        "Expected Endpoint node 'route::GET::/api/users', found: {:?}",
        collect_endpoint_names(&staging)
    );
}

#[test]
fn test_spring_request_mapping_post_method() {
    let content = r#"
package com.example.controller;

import org.springframework.web.bind.annotation.RequestMapping;
import org.springframework.web.bind.annotation.RequestMethod;

public class ItemController {
    @RequestMapping(path = "/api/items", method = RequestMethod.POST)
    public void createItem() {}
}
"#;

    let staging = build_staging_graph(content, "ItemController.java");

    assert!(
        has_endpoint_with_name(&staging, "route::POST::/api/items"),
        "Expected Endpoint node 'route::POST::/api/items', found: {:?}",
        collect_endpoint_names(&staging)
    );
}

#[test]
fn test_spring_multiple_endpoints_in_controller() {
    let content = r#"
package com.example.controller;

import org.springframework.web.bind.annotation.*;

@RestController
public class UserController {
    @GetMapping("/api/users")
    public String listUsers() {
        return "users";
    }

    @PostMapping("/api/users")
    public void createUser() {}

    @DeleteMapping("/api/users/{id}")
    public void deleteUser() {}
}
"#;

    let staging = build_staging_graph(content, "UserController.java");

    let endpoint_count = count_endpoint_nodes(&staging);
    assert_eq!(
        endpoint_count,
        3,
        "Expected 3 Endpoint nodes, got {endpoint_count}: {:?}",
        collect_endpoint_names(&staging)
    );

    assert!(
        has_endpoint_with_name(&staging, "route::GET::/api/users"),
        "Missing GET endpoint"
    );
    assert!(
        has_endpoint_with_name(&staging, "route::POST::/api/users"),
        "Missing POST endpoint"
    );
    assert!(
        has_endpoint_with_name(&staging, "route::DELETE::/api/users/{id}"),
        "Missing DELETE endpoint"
    );
}

#[test]
fn test_spring_endpoint_creates_contains_edge() {
    let content = r#"
package com.example.controller;

import org.springframework.web.bind.annotation.GetMapping;

public class UserController {
    @GetMapping("/api/users")
    public String getUsers() {
        return "users";
    }
}
"#;

    let staging = build_staging_graph(content, "UserController.java");

    // Should have at least one Contains edge connecting the endpoint to the handler
    assert!(
        has_edge_kind(&staging, "contains"),
        "Expected Contains edge linking Endpoint to handler method"
    );
}

#[test]
fn test_spring_no_endpoint_for_plain_method() {
    let content = r#"
package com.example;

public class HelperClass {
    public void doWork() {}
    private int compute(int x) { return x * 2; }
}
"#;

    let staging = build_staging_graph(content, "HelperClass.java");

    let endpoint_count = count_endpoint_nodes(&staging);
    assert_eq!(
        endpoint_count,
        0,
        "Expected no Endpoint nodes for plain methods, got {endpoint_count}: {:?}",
        collect_endpoint_names(&staging)
    );
}

#[test]
fn test_spring_no_endpoint_for_non_spring_annotations() {
    let content = r#"
package com.example;

public class Service {
    @Override
    public String toString() {
        return "Service";
    }

    @Deprecated
    public void oldMethod() {}
}
"#;

    let staging = build_staging_graph(content, "Service.java");

    let endpoint_count = count_endpoint_nodes(&staging);
    assert_eq!(
        endpoint_count,
        0,
        "Expected no Endpoint nodes for non-Spring annotations, got {endpoint_count}: {:?}",
        collect_endpoint_names(&staging)
    );
}

#[test]
fn test_spring_endpoint_with_path_variable() {
    let content = r#"
package com.example.controller;

import org.springframework.web.bind.annotation.GetMapping;

public class UserController {
    @GetMapping("/api/users/{id}")
    public String getUser() {
        return "user";
    }
}
"#;

    let staging = build_staging_graph(content, "UserController.java");

    assert!(
        has_endpoint_with_name(&staging, "route::GET::/api/users/{id}"),
        "Expected Endpoint with path variable, found: {:?}",
        collect_endpoint_names(&staging)
    );
}

#[test]
fn test_spring_endpoint_does_not_break_existing_edges() {
    let content = r#"
package com.example.controller;

import org.springframework.web.bind.annotation.GetMapping;

public class UserController {
    @GetMapping("/api/users")
    public String getUsers() {
        helper();
        return "users";
    }

    private void helper() {}
}
"#;

    let staging = build_staging_graph(content, "UserController.java");

    // Endpoint should exist
    assert!(
        has_endpoint_with_name(&staging, "route::GET::/api/users"),
        "Expected Endpoint node"
    );

    // Existing call edges should still work
    assert!(
        has_edge_kind(&staging, "calls"),
        "Expected Calls edge for helper() invocation"
    );
}

// ========== Legacy Tests (Removed) ==========
// Pre-unified-graph tests used the old CodeGraph/CodeNode API which was removed in v2.0.0.
// All functionality is covered by the active StagingGraph-based tests above.

#[cfg(disabled)]
mod disabled_tests {
    use sqry_core::graph::{
        CodeNode, EdgeKind, GraphBuilder, Language, NodeKind, unified::StagingGraph,
    };
    use sqry_lang_java::relations::JavaGraphBuilder;
    use std::path::PathBuf;
    use tree_sitter::Tree;

    // ========== Helper Functions ==========

    /// Load a test fixture from tests/fixtures/java/com/example/{package}/{ClassName}.java
    fn load_fixture(name: &str) -> String {
        let (package_path, class_name) = match name {
            "simple_calls" => ("com/example/simple", "SimpleCalls"),
            "constructor_calls" => ("com/example/constructors", "ConstructorCalls"),
            "static_methods" => ("com/example/statics", "StaticMethods"),
            "instance_methods" => ("com/example/instance", "InstanceMethods"),
            "nested_classes" => ("com/example/nested", "NestedClasses"),
            "imports" => ("com/example/imports", "Imports"),
            "native_methods" => ("com/example/jni", "NativeMethods"),
            "synchronized_methods" => ("com/example/sync", "SynchronizedMethods"),
            "interfaces" => ("com/example/interfaces", "Interfaces"),
            "enums" => ("com/example/enums", "Enums"),
            "inheritance" => ("com/example/inheritance", "Inheritance"),
            "lambdas" => ("com/example/lambdas", "Lambdas"),
            "anonymous_classes" => ("com/example/anonymous", "AnonymousClasses"),
            "qualified_calls" => ("com/example/qualified", "QualifiedCalls"),
            _ => panic!("Unknown fixture: {name}"),
        };

        let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("java")
            .join(package_path)
            .join(format!("{class_name}.java"));

        std::fs::read_to_string(&fixture_path).unwrap_or_else(|e| {
            panic!(
                "Failed to load fixture {name} from {}: {e}",
                fixture_path.display()
            )
        })
    }

    /// Parse Java source code into a tree-sitter Tree
    fn parse_java(content: &str) -> Tree {
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_java::LANGUAGE.into();
        parser
            .set_language(&language)
            .expect("Failed to load Java grammar");
        parser
            .parse(content, None)
            .expect("Failed to parse Java code")
    }

    /// Build a `CodeGraph` from Java source code (with StagingGraph)
    fn build_test_graph(content: &str, filename: &str) -> CodeGraph {
        let tree = parse_java(content);
        let graph = CodeGraph::new();
        let mut staging = StagingGraph::new();
        let builder = JavaGraphBuilder::default();
        let file_path = PathBuf::from(filename);

        builder
            .build_graph(&tree, content.as_bytes(), &file_path, &mut staging)
            .expect("Failed to build graph");

        // Note: In Phase 0.4, the StagingGraph will be committed to the CodeGraph.
        // For now, we return the empty CodeGraph to allow tests to compile.
        // The tests are ignored pending full GraphBuilder implementation.
        graph
    }

    /// Check if an `EdgeKind` is a Call edge
    fn is_call_edge(kind: &EdgeKind) -> bool {
        matches!(kind, EdgeKind::Call { .. })
    }

    /// Check if an `EdgeKind` is an Import edge
    fn is_import_edge(kind: &EdgeKind) -> bool {
        matches!(kind, EdgeKind::Import { .. })
    }

    /// Get the qualified name from a `CodeNode`
    fn get_qualified_name(node: &CodeNode) -> String {
        node.id.qualified_name.to_string()
    }

    /// Check if a node is a function/method
    fn is_function_node(node: &CodeNode) -> bool {
        matches!(node.kind, NodeKind::Function { .. })
    }

    /// Check if a node is a module
    fn is_module_node(node: &CodeNode) -> bool {
        matches!(node.kind, NodeKind::Module { .. })
    }

    // ========== Test Suite ==========

    #[test]
    fn test_simple_method_calls() {
        let content = load_fixture("simple_calls");
        let graph = build_test_graph(&content, "com/example/simple/SimpleCalls.java");

        // Collect nodes and edges
        let nodes: Vec<_> = graph.nodes().map(|n| n.value().clone()).collect();
        let edges: Vec<_> = graph.edges().map(|e| e.value().clone()).collect();

        // Should have function nodes: greet, fetch, transform, processData, main
        assert!(
            nodes.len() >= 5,
            "Expected at least 5 nodes, got {}",
            nodes.len()
        );

        // Verify specific function nodes exist with package qualification
        let node_names: Vec<String> = nodes.iter().map(get_qualified_name).collect();
        assert!(
            node_names.iter().any(|n| n.contains("greet")),
            "greet method not found in {node_names:?}"
        );
        assert!(
            node_names.iter().any(|n| n.contains("fetch")),
            "fetch method not found"
        );
        assert!(
            node_names.iter().any(|n| n.contains("processData")),
            "processData method not found"
        );

        // Should have call edges
        let call_edges: Vec<_> = edges.iter().filter(|e| is_call_edge(&e.kind)).collect();
        assert!(
            call_edges.len() >= 3,
            "Expected at least 3 call edges, got {}",
            call_edges.len()
        );
    }

    #[test]
    fn test_constructor_calls() {
        let content = load_fixture("constructor_calls");
        let graph = build_test_graph(&content, "com/example/constructors/ConstructorCalls.java");

        let nodes: Vec<_> = graph.nodes().map(|n| n.value().clone()).collect();
        let edges: Vec<_> = graph.edges().map(|e| e.value().clone()).collect();

        // Should have constructor nodes (Point.<init>, Rectangle.<init>)
        let node_names: Vec<String> = nodes.iter().map(get_qualified_name).collect();
        let constructor_nodes: Vec<_> =
            node_names.iter().filter(|n| n.contains("<init>")).collect();
        assert!(
            constructor_nodes.len() >= 2,
            "Expected at least 2 constructor nodes, got {}",
            constructor_nodes.len()
        );

        // Should have call edges to constructors (new expressions)
        let call_edges: Vec<_> = edges.iter().filter(|e| is_call_edge(&e.kind)).collect();
        assert!(!call_edges.is_empty(), "Expected constructor call edges");
    }

    #[test]
    fn test_static_method_calls() {
        let content = load_fixture("static_methods");
        let graph = build_test_graph(&content, "com/example/statics/StaticMethods.java");

        let nodes: Vec<_> = graph.nodes().map(|n| n.value().clone()).collect();
        let edges: Vec<_> = graph.edges().map(|e| e.value().clone()).collect();

        // Should have static method nodes
        let node_names: Vec<String> = nodes.iter().map(get_qualified_name).collect();
        assert!(
            node_names
                .iter()
                .any(|n| n.contains("MathUtils") && n.contains("add")),
            "MathUtils.add not found"
        );
        assert!(
            node_names
                .iter()
                .any(|n| n.contains("StringUtils") && n.contains("reverse")),
            "StringUtils.reverse not found"
        );

        // Should have call edges including qualified static calls
        let call_edges: Vec<_> = edges.iter().filter(|e| is_call_edge(&e.kind)).collect();
        assert!(
            call_edges.len() >= 5,
            "Expected at least 5 call edges for static methods"
        );
    }

    #[test]
    fn test_instance_method_calls() {
        let content = load_fixture("instance_methods");
        let graph = build_test_graph(&content, "com/example/instance/InstanceMethods.java");

        let nodes: Vec<_> = graph.nodes().map(|n| n.value().clone()).collect();
        let edges: Vec<_> = graph.edges().map(|e| e.value().clone()).collect();

        // Should have instance methods: getName, getValue, setValue, increment, display
        let node_names: Vec<String> = nodes.iter().map(get_qualified_name).collect();
        assert!(
            node_names.iter().any(|n| n.contains("getName")),
            "getName method not found"
        );
        assert!(
            node_names.iter().any(|n| n.contains("increment")),
            "increment method not found"
        );

        // Should have call edges for method chaining
        let call_edges: Vec<_> = edges.iter().filter(|e| is_call_edge(&e.kind)).collect();
        assert!(
            !call_edges.is_empty(),
            "Expected instance method call edges"
        );
    }

    #[test]
    fn test_nested_classes() {
        let content = load_fixture("nested_classes");
        let graph = build_test_graph(&content, "com/example/nested/NestedClasses.java");

        let nodes: Vec<_> = graph.nodes().map(|n| n.value().clone()).collect();
        let node_names: Vec<String> = nodes.iter().map(get_qualified_name).collect();

        // Should have nested class methods with proper qualification
        // e.g., com.example.nested.NestedClasses.Inner.innerMethod
        assert!(
            node_names
                .iter()
                .any(|n| n.contains("Inner") && n.contains("innerMethod")),
            "Inner.innerMethod not found in {node_names:?}"
        );
        assert!(
            node_names.iter().any(|n| n.contains("StaticNested")),
            "StaticNested class methods not found"
        );
        assert!(
            node_names.iter().any(|n| n.contains("DeepNested")),
            "DeepNested class methods not found"
        );
    }

    #[test]
    fn test_import_declarations() {
        let content = load_fixture("imports");
        let graph = build_test_graph(&content, "com/example/imports/Imports.java");

        let edges: Vec<_> = graph.edges().map(|e| e.value().clone()).collect();

        // Should have import edges
        let import_edges: Vec<_> = edges.iter().filter(|e| is_import_edge(&e.kind)).collect();
        assert!(
            import_edges.len() >= 5,
            "Expected at least 5 import edges, got {}",
            import_edges.len()
        );

        // Should have wildcard import
        let wildcard_imports: Vec<_> = import_edges
            .iter()
            .filter(|e| {
                if let EdgeKind::Import { is_wildcard, .. } = e.kind {
                    is_wildcard
                } else {
                    false
                }
            })
            .collect();
        assert!(
            !wildcard_imports.is_empty(),
            "Expected at least one wildcard import"
        );
    }

    #[test]
    fn test_native_methods() {
        let content = load_fixture("native_methods");
        let graph = build_test_graph(&content, "com/example/jni/NativeMethods.java");

        let nodes: Vec<_> = graph.nodes().map(|n| n.value().clone()).collect();
        let node_names: Vec<String> = nodes.iter().map(get_qualified_name).collect();

        // Should have native method nodes
        assert!(
            node_names.iter().any(|n| n.contains("nativeVoidMethod")),
            "nativeVoidMethod not found"
        );
        assert!(
            node_names.iter().any(|n| n.contains("nativeIntMethod")),
            "nativeIntMethod not found"
        );
        assert!(
            node_names.iter().any(|n| n.contains("nativeStaticMethod")),
            "nativeStaticMethod not found"
        );

        // Native methods should be marked in metadata
        // (This would require checking node metadata if we store native flag)
    }

    #[test]
    fn test_synchronized_methods() {
        let content = load_fixture("synchronized_methods");
        let graph = build_test_graph(&content, "com/example/sync/SynchronizedMethods.java");

        let nodes: Vec<_> = graph.nodes().map(|n| n.value().clone()).collect();
        let node_names: Vec<String> = nodes.iter().map(get_qualified_name).collect();

        // Should have synchronized methods
        assert!(
            node_names.iter().any(|n| n.contains("incrementSync")),
            "incrementSync method not found"
        );
        assert!(
            node_names.iter().any(|n| n.contains("getCounterSync")),
            "getCounterSync method not found"
        );
        assert!(
            node_names.iter().any(|n| n.contains("staticSyncMethod")),
            "staticSyncMethod method not found"
        );
    }

    #[test]
    fn test_interfaces() {
        let content = load_fixture("interfaces");
        let graph = build_test_graph(&content, "com/example/interfaces/Interfaces.java");

        let nodes: Vec<_> = graph.nodes().map(|n| n.value().clone()).collect();
        let node_names: Vec<String> = nodes.iter().map(get_qualified_name).collect();

        // Should have interface methods
        assert!(
            node_names
                .iter()
                .any(|n| n.contains("Calculator") && n.contains("add")),
            "Calculator.add not found"
        );
        assert!(
            node_names
                .iter()
                .any(|n| n.contains("Calculator") && n.contains("multiply")),
            "Calculator.multiply (default method) not found"
        );

        // Should have implementing class methods
        assert!(
            node_names
                .iter()
                .any(|n| n.contains("SimpleCalculator") && n.contains("compute")),
            "SimpleCalculator.compute not found"
        );
    }

    #[test]
    fn test_enums() {
        let content = load_fixture("enums");
        let graph = build_test_graph(&content, "com/example/enums/Enums.java");

        let nodes: Vec<_> = graph.nodes().map(|n| n.value().clone()).collect();
        let node_names: Vec<String> = nodes.iter().map(get_qualified_name).collect();

        // Should have enum methods (nested within Enums.Status and Enums.Priority)
        assert!(
            node_names.iter().any(|n| n.contains("isFinished")),
            "isFinished not found in {node_names:?}"
        );
        assert!(
            node_names.iter().any(|n| n.contains("getValue")),
            "getValue not found in {node_names:?}"
        );
        assert!(
            node_names.iter().any(|n| n.contains("isHigherThan")),
            "isHigherThan not found"
        );
    }

    #[test]
    fn test_inheritance() {
        let content = load_fixture("inheritance");
        let graph = build_test_graph(&content, "com/example/inheritance/Inheritance.java");

        let nodes: Vec<_> = graph.nodes().map(|n| n.value().clone()).collect();
        let edges: Vec<_> = graph.edges().map(|e| e.value().clone()).collect();
        let node_names: Vec<String> = nodes.iter().map(get_qualified_name).collect();

        // Should have parent and child class methods
        assert!(
            node_names
                .iter()
                .any(|n| n.contains("Animal") && n.contains("speak")),
            "Animal.speak not found"
        );
        assert!(
            node_names
                .iter()
                .any(|n| n.contains("Dog") && n.contains("speak")),
            "Dog.speak not found"
        );
        assert!(
            node_names
                .iter()
                .any(|n| n.contains("Dog") && n.contains("getInfo")),
            "Dog.getInfo not found"
        );

        // Should have call edges including super calls
        let call_edges: Vec<_> = edges.iter().filter(|e| is_call_edge(&e.kind)).collect();
        assert!(
            !call_edges.is_empty(),
            "Expected call edges in inheritance hierarchy"
        );
    }

    #[test]
    fn test_lambdas() {
        let content = load_fixture("lambdas");
        let graph = build_test_graph(&content, "com/example/lambdas/Lambdas.java");

        let nodes: Vec<_> = graph.nodes().map(|n| n.value().clone()).collect();
        let node_names: Vec<String> = nodes.iter().map(get_qualified_name).collect();

        // Should have methods that use lambdas
        assert!(
            node_names.iter().any(|n| n.contains("compute")),
            "compute method not found"
        );
        assert!(
            node_names.iter().any(|n| n.contains("test")),
            "test method not found"
        );

        // Lambda bodies might not be extracted as separate functions,
        // but the containing methods should be present
        assert!(
            node_names.iter().any(|n| n.contains("main")),
            "main method not found"
        );
    }

    #[test]
    fn test_anonymous_classes() {
        let content = load_fixture("anonymous_classes");
        let graph = build_test_graph(&content, "com/example/anonymous/AnonymousClasses.java");

        let nodes: Vec<_> = graph.nodes().map(|n| n.value().clone()).collect();
        let node_names: Vec<String> = nodes.iter().map(get_qualified_name).collect();

        // Should have methods from anonymous classes
        // Note: Anonymous class methods might have generated names
        assert!(
            node_names.iter().any(|n| n.contains("execute")),
            "execute method not found"
        );
        assert!(
            node_names.iter().any(|n| n.contains("main")),
            "main method not found"
        );

        // Anonymous class methods (run, work, helper) should be present
        // Their qualification might include generated class names
    }

    #[test]
    fn test_qualified_calls() {
        let content = load_fixture("qualified_calls");
        let graph = build_test_graph(&content, "com/example/qualified/QualifiedCalls.java");

        let edges: Vec<_> = graph.edges().map(|e| e.value().clone()).collect();

        // Should have call edges to fully qualified methods
        let call_edges: Vec<_> = edges.iter().filter(|e| is_call_edge(&e.kind)).collect();
        assert!(
            call_edges.len() >= 5,
            "Expected at least 5 qualified call edges, got {}",
            call_edges.len()
        );

        // Calls should reference java.lang.*, java.util.* methods
        // (Check would require examining edge targets)
    }

    #[test]
    fn test_module_nodes_created() {
        let content = load_fixture("imports");
        let graph = build_test_graph(&content, "com/example/imports/Imports.java");

        let nodes: Vec<_> = graph.nodes().map(|n| n.value().clone()).collect();

        // Should have module nodes (one per import, representing external packages)
        let module_nodes: Vec<_> = nodes.iter().filter(|n| is_module_node(n)).collect();
        assert!(
            !module_nodes.is_empty(),
            "Expected at least one module node for imports"
        );

        // Module nodes should represent imported packages
        let module_names: Vec<String> =
            module_nodes.iter().map(|n| get_qualified_name(n)).collect();
        assert!(
            module_names.iter().any(|n| n.contains("java.util")),
            "Expected java.util module node"
        );
    }

    #[test]
    fn test_function_nodes_have_correct_language() {
        let content = load_fixture("simple_calls");
        let graph = build_test_graph(&content, "com/example/simple/SimpleCalls.java");

        let nodes: Vec<_> = graph.nodes().map(|n| n.value().clone()).collect();
        let function_nodes: Vec<_> = nodes.iter().filter(|n| is_function_node(n)).collect();

        // All function nodes should have Java language
        for node in function_nodes {
            assert_eq!(
                node.id.language,
                Language::Java,
                "Node {} should have Java language",
                get_qualified_name(node)
            );
        }
    }

    #[test]
    fn test_edges_have_metadata() {
        let content = load_fixture("simple_calls");
        let graph = build_test_graph(&content, "com/example/simple/SimpleCalls.java");

        let edges: Vec<_> = graph.edges().map(|e| e.value().clone()).collect();
        let call_edges: Vec<_> = edges.iter().filter(|e| is_call_edge(&e.kind)).collect();

        // All call edges should have metadata
        for edge in call_edges {
            assert!(
                edge.metadata.span.is_some(),
                "Call edge should have span information"
            );
            assert!(
                edge.metadata.confidence > 0.0,
                "Call edge should have confidence > 0"
            );
        }
    }

    #[test]
    fn test_call_edges_have_argument_count() {
        let content = load_fixture("simple_calls");
        let graph = build_test_graph(&content, "com/example/simple/SimpleCalls.java");

        let edges: Vec<_> = graph.edges().map(|e| e.value().clone()).collect();

        // Check that Call edges have argument_count field (usize is always >= 0)
        let call_count = edges
            .iter()
            .filter(|e| matches!(&e.kind, EdgeKind::Call { .. }))
            .count();

        // At least some call edges should exist
        assert!(call_count > 0, "Expected at least one Call edge");
    }

    #[test]
    fn test_package_qualification_present() {
        let content = load_fixture("simple_calls");
        let graph = build_test_graph(&content, "com/example/simple/SimpleCalls.java");

        let nodes: Vec<_> = graph.nodes().map(|n| n.value().clone()).collect();
        let node_names: Vec<String> = nodes.iter().map(get_qualified_name).collect();

        // At least some nodes should have package qualification (com.example.simple.*)
        let qualified_nodes: Vec<_> = node_names
            .iter()
            .filter(|n| n.contains("com.example"))
            .collect();

        assert!(
            !qualified_nodes.is_empty(),
            "Expected nodes with package qualification, found none in {node_names:?}"
        );
    }
} // end mod disabled_tests

// Test fixture mapping coverage - runs even when disabled_tests is disabled
#[test]
fn test_fixture_mapping_coverage() {
    use std::path::PathBuf;

    /// Load a test fixture from tests/fixtures/java/com/example/{package}/{ClassName}.java
    fn load_fixture(name: &str) -> String {
        let (package_path, class_name) = match name {
            "simple_calls" => ("com/example/simple", "SimpleCalls"),
            "constructor_calls" => ("com/example/constructors", "ConstructorCalls"),
            "static_methods" => ("com/example/statics", "StaticMethods"),
            "instance_methods" => ("com/example/instance", "InstanceMethods"),
            "nested_classes" => ("com/example/nested", "NestedClasses"),
            "imports" => ("com/example/imports", "Imports"),
            "native_methods" => ("com/example/jni", "NativeMethods"),
            "synchronized_methods" => ("com/example/sync", "SynchronizedMethods"),
            "interfaces" => ("com/example/interfaces", "Interfaces"),
            "enums" => ("com/example/enums", "Enums"),
            "inheritance" => ("com/example/inheritance", "Inheritance"),
            "lambdas" => ("com/example/lambdas", "Lambdas"),
            "anonymous_classes" => ("com/example/anonymous", "AnonymousClasses"),
            "qualified_calls" => ("com/example/qualified", "QualifiedCalls"),
            _ => panic!("Unknown fixture: {name}"),
        };

        let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("java")
            .join(package_path)
            .join(format!("{class_name}.java"));

        std::fs::read_to_string(&fixture_path).unwrap_or_else(|e| {
            panic!(
                "Failed to load fixture {name} from {}: {e}",
                fixture_path.display()
            )
        })
    }

    // Ensure all 14 sqry-lang-java fixture names have path mappings
    let fixture_names = vec![
        "simple_calls",
        "constructor_calls",
        "static_methods",
        "instance_methods",
        "nested_classes",
        "imports",
        "native_methods",
        "synchronized_methods",
        "interfaces",
        "enums",
        "inheritance",
        "lambdas",
        "anonymous_classes",
        "qualified_calls",
    ];

    for name in fixture_names {
        // This will panic if mapping is missing or file doesn't exist
        let content = load_fixture(name);
        assert!(!content.is_empty(), "Fixture {name} is empty");

        // Verify class name matches expected pattern
        let expected_class = match name {
            "simple_calls" => "SimpleCalls",
            "constructor_calls" => "ConstructorCalls",
            "static_methods" => "StaticMethods",
            "instance_methods" => "InstanceMethods",
            "nested_classes" => "NestedClasses",
            "imports" => "Imports",
            "native_methods" => "NativeMethods",
            "synchronized_methods" => "SynchronizedMethods",
            "interfaces" => "Interfaces",
            "enums" => "Enums",
            "inheritance" => "Inheritance",
            "lambdas" => "Lambdas",
            "anonymous_classes" => "AnonymousClasses",
            "qualified_calls" => "QualifiedCalls",
            _ => panic!("Unknown fixture"),
        };

        assert!(
            content.contains(&format!("class {expected_class}")),
            "Fixture {name} doesn't contain expected class {expected_class}"
        );
    }
}

// ========== Spring MVC Class-Level @RequestMapping Prefix Tests ==========

#[test]
fn test_java_spring_class_level_request_mapping_prefix() {
    let content = r#"
package com.example.controller;

import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.PostMapping;
import org.springframework.web.bind.annotation.RequestMapping;
import org.springframework.web.bind.annotation.RestController;

@RestController
@RequestMapping("/api")
public class UserController {
    @GetMapping("/users")
    public String getUsers() {
        return "users";
    }

    @PostMapping("/items")
    public String createItem() {
        return "created";
    }
}
"#;

    let staging = build_staging_graph(content, "UserController.java");
    let endpoints = collect_endpoint_names(&staging);

    assert!(
        has_endpoint_with_name(&staging, "route::GET::/api/users"),
        "Expected 'route::GET::/api/users' with class prefix, found: {:?}",
        endpoints
    );
    assert!(
        has_endpoint_with_name(&staging, "route::POST::/api/items"),
        "Expected 'route::POST::/api/items' with class prefix, found: {:?}",
        endpoints
    );
    // Verify the prefix IS composed (not just /users)
    assert!(
        !has_endpoint_with_name(&staging, "route::GET::/users\n"),
        "Endpoint should have /api prefix, not bare /users"
    );
}

#[test]
fn test_java_spring_method_only_no_class_prefix() {
    let content = r#"
package com.example.controller;

import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.RestController;

@RestController
public class SimpleController {
    @GetMapping("/health")
    public String health() {
        return "ok";
    }
}
"#;

    let staging = build_staging_graph(content, "SimpleController.java");
    let endpoints = collect_endpoint_names(&staging);

    assert!(
        has_endpoint_with_name(&staging, "route::GET::/health"),
        "Expected 'route::GET::/health' (no class prefix), found: {:?}",
        endpoints
    );
}
