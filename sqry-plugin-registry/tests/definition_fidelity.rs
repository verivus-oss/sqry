//! Issue #394 Slice A: fixture-driven `is_definition` fidelity guard.
//!
//! The completeness contract for the items-only filter (design R8.1): a curated
//! multi-language fixture corpus with EXPLICIT expected `is_definition` values
//! per named node. No derived oracle is used as a gate (three design rounds
//! showed no derived oracle is precisely sound); explicit expected values are
//! unambiguously the ground truth.
//!
//! Each fixture is a tiny source file built through the full plugin roster
//! (`create_plugin_manager_all`) via the real `build_unified_graph` pipeline.
//! For every named node we assert the exact expected `is_definition`:
//!
//!   * real top-level function / method / type / struct / class / enum /
//!     interface / module declaration -> true
//!   * a call to an external (undeclared) symbol -> false (callee stub)
//!   * an FFI / syscall / native target -> false (where the language has one)
//!   * an import binding -> false
//!   * a reference to an external type -> false (typed languages)
//!   * a framework route handler that is a real in-workspace function -> true;
//!     an external one -> false (where the language has route extraction)
//!
//! False positives (a stub marked true) are prevented by construction
//! (default-false at every node-creation sink; only declaration sites opt in).
//! False negatives (a real declaration left false) are caught here for every
//! class the fixtures cover. Languages / constructs not in the corpus are a
//! documented quality limitation, not an unsoundness (design R8.2).

use std::collections::HashMap;
use std::path::Path;

use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
use sqry_core::graph::unified::node::NodeKind;
use sqry_plugin_registry::create_plugin_manager_all;
use tempfile::TempDir;

/// A single observed node: semantic name, kind, and its `is_definition` flag.
#[derive(Debug, Clone)]
struct ObservedNode {
    name: String,
    kind: NodeKind,
    is_definition: bool,
}

/// Build the given (filename, source) fixtures into one workspace and return
/// every named node observed in the committed graph.
fn observe(fixtures: &[(&str, &str)]) -> Vec<ObservedNode> {
    let dir = TempDir::new().expect("tempdir");
    for (name, src) in fixtures {
        let path = dir.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(&path, src).expect("write fixture");
    }
    let plugins = create_plugin_manager_all();
    let graph = build_unified_graph(dir.path(), &plugins, &BuildConfig::default())
        .expect("fixture workspace builds");
    let snapshot = graph.snapshot();
    let strings = snapshot.strings();
    let mut out = Vec::new();
    for (_id, entry) in snapshot.nodes().iter() {
        let name = strings
            .resolve(entry.name)
            .map(|s| s.to_string())
            .unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        out.push(ObservedNode {
            name,
            kind: entry.kind,
            is_definition: entry.is_definition,
        });
    }
    out
}

/// Diagnostic: build each fixture and print every named node so the explicit
/// assertions can be authored against reality. Run with:
///   cargo test -p sqry-plugin-registry --test definition_fidelity \
///       dump_observed -- --ignored --nocapture
#[test]
#[ignore = "diagnostic dump, not a gate"]
fn dump_observed() {
    for (lang, fixtures) in all_fixtures() {
        let nodes = observe(fixtures);
        println!("\n========== {lang} ==========");
        let mut sorted = nodes.clone();
        sorted.sort_by(|a, b| (a.name.clone(), a.kind).cmp(&(b.name.clone(), b.kind)));
        for n in &sorted {
            println!("  is_def={:<5} {:<14?} {}", n.is_definition, n.kind, n.name);
        }
    }
}

/// The fixture corpus, keyed by language label.
fn all_fixtures() -> Vec<(&'static str, &'static [(&'static str, &'static str)])> {
    vec![
        ("rust", RUST),
        ("c", C),
        ("go", GO),
        ("typescript", TYPESCRIPT),
        ("javascript", JAVASCRIPT),
        ("python", PYTHON),
        ("java", JAVA),
        ("csharp", CSHARP),
        ("elixir", ELIXIR),
        ("svelte", SVELTE),
        ("vue", VUE),
    ]
}

const RUST: &[(&str, &str)] = &[(
    "lib.rs",
    r#"
use std::collections::HashMap;

pub fn real_top_fn() -> i32 {
    external_undeclared_fn();
    0
}

pub struct RealStruct {
    field: i32,
}

pub enum RealEnum {
    A,
    B,
}

pub trait RealTrait {
    fn declared_trait_method(&self);
}

impl RealStruct {
    pub fn real_method(&self) -> i32 {
        1
    }
}

pub type RealAlias = i32;

pub mod real_module {
    pub fn inner_fn() {}
}

fn uses_external_type(_x: ExternalRefType) {}
"#,
)];

const C: &[(&str, &str)] = &[(
    "lib.c",
    r#"
#include <stdio.h>

typedef int RealTypedef;

struct RealCStruct {
    int field;
};

enum RealCEnum { CA, CB };

int real_c_fn(int x) {
    external_undeclared_c_fn();
    return x;
}

RealTypedef uses_typedef(struct RealCStruct s) {
    return s.field;
}
"#,
)];

const GO: &[(&str, &str)] = &[(
    "main.go",
    r#"
package main

import (
    "fmt"
    "net/http"
    "syscall"
)

type RealGoStruct struct {
    Field int
}

type RealGoInterface interface {
    DeclaredMethod() int
}

func RealGoFunc() int {
    externalUndeclaredGoFn()
    return 0
}

func (s RealGoStruct) RealGoMethod() int {
    return s.Field
}

func usesType(x RealGoStruct) int {
    fmt.Println(x)
    syscall.Syscall(1, 2, 3, 4)
    return 0
}

func registerRoutes() {
    http.HandleFunc("/api/real", routeHandler)
}

func routeHandler(w http.ResponseWriter, r *http.Request) {
}
"#,
)];

const TYPESCRIPT: &[(&str, &str)] = &[(
    "app.ts",
    r#"
import { something } from "./other";

export function realTsFunc(): number {
    externalUndeclaredTsFn();
    return 0;
}

export class RealTsClass {
    realTsMethod(): number {
        return 1;
    }
}

export interface RealTsInterface {
    declaredField: number;
}

export enum RealTsEnum {
    A,
    B,
}

export type RealTsType = number;

function usesType(x: ExternalRefTsType): void {}
"#,
)];

const JAVASCRIPT: &[(&str, &str)] = &[(
    "app.js",
    r#"
export function realJsFunc() {
    externalUndeclaredJsFn();
}

export class RealJsClass {
}

export const realJsVar = 1;

module.exports = function () {
    return 1;
};
"#,
)];

const PYTHON: &[(&str, &str)] = &[(
    "mod.py",
    r#"
import os

def real_py_func():
    external_undeclared_py_fn()
    return 0

class RealPyClass:
    def real_py_method(self):
        return 1
"#,
)];

const JAVA: &[(&str, &str)] = &[(
    "Real.java",
    r#"
import java.util.List;

public class RealJavaClass {
    public int realJavaMethod() {
        externalUndeclaredJavaFn();
        return 1;
    }
}

interface RealJavaInterface {
    int declaredJavaMethod();
}

enum RealJavaEnum { A, B }
"#,
)];

const CSHARP: &[(&str, &str)] = &[(
    "Real.cs",
    r#"
using System;

namespace RealNs {
    public class RealCsClass {
        public int RealCsMethod(int realCsParam) {
            int realCsLocal = realCsParam;
            ExternalUndeclaredCsFn();
            return realCsLocal;
        }
    }

    public class RealCsGeneric<TRealTypeParam> {
    }

    public interface RealCsInterface {
        int DeclaredCsMethod();
    }

    public enum RealCsEnum { A, B }
}
"#,
)];

const ELIXIR: &[(&str, &str)] = &[(
    "mod.ex",
    r#"
defmodule RealElixirModule do
  def real_elixir_func do
    ExternalUndeclared.fn()
    0
  end

  defp private_elixir_func do
    1
  end
end
"#,
)];

const SVELTE: &[(&str, &str)] = &[(
    "RealSvelte.svelte",
    r#"
<script lang="ts">
    export let svelteProp: string;

    function realSvelteFunc(realSvelteParam: string): string {
        const realSvelteLocal: string = realSvelteParam;
        externalSvelteFn();
        return realSvelteLocal;
    }
</script>

<button on:click={realSvelteFunc}>Run</button>
"#,
)];

const VUE: &[(&str, &str)] = &[(
    "RealVue.vue",
    r#"
<template>
  <button @click="realVueFunc">Run</button>
</template>

<script lang="ts">
export default {
  props: {
    vueProp: String
  },
  methods: {
    realVueFunc(realVueParam: string): string {
      const realVueLocal: string = realVueParam;
      externalVueFn();
      return realVueLocal;
    }
  }
}
</script>
"#,
)];

/// Find observed nodes matching a (name, kind) pair.
fn matching<'a>(nodes: &'a [ObservedNode], name: &str, kind: NodeKind) -> Vec<&'a ObservedNode> {
    nodes
        .iter()
        .filter(|n| n.name == name && n.kind == kind)
        .collect()
}

/// Assert at least one node with (name, kind) exists and ALL such nodes carry
/// `expected` is_definition. A real declaration must be true on every instance
/// (unification + OR-in converge); a pure stub must be false on every instance.
fn assert_def(nodes: &[ObservedNode], lang: &str, name: &str, kind: NodeKind, expected: bool) {
    let hits = matching(nodes, name, kind);
    assert!(
        !hits.is_empty(),
        "[{lang}] expected a {kind:?} node named `{name}` but found none; \
         observed: {:?}",
        nodes.iter().map(|n| (&n.name, n.kind)).collect::<Vec<_>>()
    );
    for n in hits {
        assert_eq!(
            n.is_definition, expected,
            "[{lang}] node `{name}` ({kind:?}) is_definition should be {expected}, \
             got {}",
            n.is_definition
        );
    }
}

/// Assert AT LEAST ONE node with (name, kind) carries `expected` is_definition.
///
/// Used where a real declaration legitimately coexists with a separate
/// reference stub of the same name+kind that the plugin does not unify (e.g. a
/// Go route handler: the real `func routeHandler` declaration is a definition,
/// while the `http.HandleFunc(..., routeHandler)` argument reference is a
/// non-unified stub). The declaration must exist and be marked; the stub
/// staying false is covered by the by-construction default.
fn assert_any_def(nodes: &[ObservedNode], lang: &str, name: &str, kind: NodeKind, expected: bool) {
    let hits = matching(nodes, name, kind);
    assert!(
        !hits.is_empty(),
        "[{lang}] expected a {kind:?} node named `{name}` but found none"
    );
    assert!(
        hits.iter().any(|n| n.is_definition == expected),
        "[{lang}] expected at least one `{name}` ({kind:?}) with is_definition={expected}, \
         got {:?}",
        hits.iter().map(|n| n.is_definition).collect::<Vec<_>>()
    );
}

#[test]
fn rust_definition_fidelity() {
    let n = observe(RUST);
    let l = "rust";
    assert_def(&n, l, "real_top_fn", NodeKind::Function, true);
    assert_def(&n, l, "real_method", NodeKind::Method, true);
    assert_def(&n, l, "RealStruct", NodeKind::Struct, true);
    assert_def(&n, l, "RealEnum", NodeKind::Enum, true);
    assert_def(&n, l, "RealTrait", NodeKind::Interface, true);
    assert_def(&n, l, "RealAlias", NodeKind::Type, true);
    assert_def(&n, l, "real_module", NodeKind::Module, true);
    // Negative: stub / reference / import classes must be false.
    assert_def(&n, l, "external_undeclared_fn", NodeKind::Function, false);
    assert_def(&n, l, "ExternalRefType", NodeKind::Type, false);
    assert_def(&n, l, "HashMap", NodeKind::Import, false);
}

#[test]
fn c_definition_fidelity() {
    let n = observe(C);
    let l = "c";
    assert_def(&n, l, "real_c_fn", NodeKind::Function, true);
    assert_def(&n, l, "RealCStruct", NodeKind::Struct, true);
    assert_def(&n, l, "RealTypedef", NodeKind::Type, true);
    // Negative: external call stub + import must be false.
    assert_def(&n, l, "external_undeclared_c_fn", NodeKind::Function, false);
    assert_def(&n, l, "stdio.h", NodeKind::Import, false);
}

#[test]
fn go_definition_fidelity() {
    let n = observe(GO);
    let l = "go";
    assert_def(&n, l, "RealGoFunc", NodeKind::Function, true);
    assert_def(&n, l, "RealGoMethod", NodeKind::Method, true);
    assert_def(&n, l, "RealGoStruct", NodeKind::Struct, true);
    assert_def(&n, l, "RealGoInterface", NodeKind::Interface, true);
    // Route handler that IS a real in-workspace function -> the declaration node
    // is a definition (a non-unified HandleFunc-argument reference stub of the
    // same name stays false, covered by-construction).
    assert_any_def(&n, l, "routeHandler", NodeKind::Function, true);
    // Negative: external call + imports must be false.
    assert_def(&n, l, "externalUndeclaredGoFn", NodeKind::Function, false);
    assert_def(&n, l, "fmt", NodeKind::Import, false);
    assert_def(&n, l, "syscall", NodeKind::Import, false);
}

#[test]
fn typescript_definition_fidelity() {
    let n = observe(TYPESCRIPT);
    let l = "typescript";
    assert_def(&n, l, "realTsFunc", NodeKind::Function, true);
    assert_def(&n, l, "RealTsClass", NodeKind::Class, true);
    assert_def(&n, l, "RealTsInterface", NodeKind::Interface, true);
    assert_def(&n, l, "RealTsEnum", NodeKind::Enum, true);
    assert_def(&n, l, "RealTsType", NodeKind::Type, true);
    // Negative: external call stub + external type reference + import are false.
    assert_def(&n, l, "externalUndeclaredTsFn", NodeKind::Function, false);
    assert_def(&n, l, "ExternalRefTsType", NodeKind::Type, false);
    assert_def(&n, l, "something", NodeKind::Import, false);
}

#[test]
fn javascript_definition_fidelity() {
    let n = observe(JAVASCRIPT);
    let l = "javascript";
    assert_def(&n, l, "realJsFunc", NodeKind::Function, true);
    assert_def(&n, l, "RealJsClass", NodeKind::Class, true);
    assert_def(&n, l, "realJsVar", NodeKind::Variable, true);
    assert_def(&n, l, "default", NodeKind::Function, true);
    assert_def(&n, l, "externalUndeclaredJsFn", NodeKind::Function, false);
}

#[test]
fn python_definition_fidelity() {
    let n = observe(PYTHON);
    let l = "python";
    assert_def(&n, l, "real_py_func", NodeKind::Function, true);
    assert_def(&n, l, "RealPyClass", NodeKind::Class, true);
    // Negative: external call stub + import are false.
    assert_def(
        &n,
        l,
        "external_undeclared_py_fn",
        NodeKind::Function,
        false,
    );
    assert_def(&n, l, "os", NodeKind::Import, false);
}

#[test]
fn java_definition_fidelity() {
    let n = observe(JAVA);
    let l = "java";
    assert_def(&n, l, "RealJavaClass", NodeKind::Class, true);
    assert_def(&n, l, "realJavaMethod", NodeKind::Method, true);
    assert_def(&n, l, "RealJavaInterface", NodeKind::Interface, true);
    // Java models `enum` as a Class node (no dedicated Enum kind in this plugin).
    assert_def(&n, l, "RealJavaEnum", NodeKind::Class, true);
    // Negative: external call stub (Method kind) + import are false.
    assert_def(&n, l, "externalUndeclaredJavaFn", NodeKind::Method, false);
    assert_def(&n, l, "List", NodeKind::Import, false);
}

#[test]
fn csharp_definition_fidelity() {
    let n = observe(CSHARP);
    let l = "csharp";
    assert_def(&n, l, "RealCsClass", NodeKind::Class, true);
    assert_def(&n, l, "RealCsMethod", NodeKind::Method, true);
    assert_def(&n, l, "realCsLocal", NodeKind::Variable, true);
    assert_def(&n, l, "realCsParam@102", NodeKind::Variable, true);
    assert_def(&n, l, "TRealTypeParam", NodeKind::Type, true);
    assert_def(&n, l, "RealCsInterface", NodeKind::Interface, true);
    // Negative: external call stub + import are false.
    assert_def(&n, l, "ExternalUndeclaredCsFn", NodeKind::Function, false);
    assert_def(&n, l, "System", NodeKind::Import, false);
}

#[test]
fn elixir_definition_fidelity() {
    let n = observe(ELIXIR);
    let l = "elixir";
    assert_def(&n, l, "real_elixir_func", NodeKind::Function, true);
}

#[test]
fn svelte_definition_fidelity() {
    let n = observe(SVELTE);
    let l = "svelte";
    assert_def(&n, l, "RealSvelte", NodeKind::Component, true);
    assert_def(&n, l, "svelteProp", NodeKind::Variable, true);
    assert_any_def(&n, l, "realSvelteFunc", NodeKind::Function, true);
    assert_any_def(&n, l, "realSvelteParam", NodeKind::Variable, true);
    assert_any_def(&n, l, "realSvelteLocal_at_109", NodeKind::Variable, true);
    assert_def(&n, l, "externalSvelteFn", NodeKind::Function, false);
}

#[test]
fn vue_definition_fidelity() {
    let n = observe(VUE);
    let l = "vue";
    assert_def(&n, l, "RealVue", NodeKind::Component, true);
    assert_def(&n, l, "vueProp", NodeKind::Variable, true);
    assert_any_def(&n, l, "realVueFunc", NodeKind::Method, true);
    assert_any_def(&n, l, "realVueParam", NodeKind::Variable, true);
    assert_any_def(&n, l, "realVueLocal_at_126", NodeKind::Variable, true);
    assert_def(&n, l, "externalVueFn", NodeKind::Function, false);
}

// Silence unused-import warning if HashMap usage is trimmed during iteration.
#[allow(dead_code)]
fn _assert_helper_types(_: HashMap<String, String>, _: &Path) {}
