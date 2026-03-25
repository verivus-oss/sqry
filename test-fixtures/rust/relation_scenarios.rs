//! Test fixture for FR-RUST relation tracking scenarios
//! Covers all 16 scenarios from the implementation plan

// ========== SC-01: pub fn exported() ==========
pub fn exported_function() {
    println!("I am exported");
}

// ========== SC-02: pub struct Config ==========
pub struct Config {
    pub name: String,
    pub value: i32,
}

// ========== SC-03: pub trait Handler ==========
pub trait Handler {
    fn handle(&self);
}

// ========== SC-04: pub(crate) fn internal() ==========
pub(crate) fn internal_function() {
    println!("I am crate-internal");
}

// ========== SC-05: pub use other::Item ==========
mod other {
    pub struct Item;
    pub fn helper() {}
}
pub use other::Item;

// ========== SC-06: pub use path::* (glob) ==========
mod utils {
    pub fn util_a() {}
    pub fn util_b() {}
}
pub use utils::*;

// ========== SC-07: pub use path::{self, A, B} ==========
mod components {
    pub struct ComponentA;
    pub struct ComponentB;
    pub fn init() {}
}
pub use components::{self as comp, ComponentA, ComponentB};

// ========== SC-08: point.x (struct field access) ==========
struct Point {
    x: i32,
    y: i32,
}

fn access_struct_field() {
    let point = Point { x: 10, y: 20 };
    let _x = point.x;
    let _y = point.y;
}

// ========== SC-09: tuple.0 (tuple field access) ==========
fn access_tuple_field() {
    let tuple = (1, 2, 3);
    let _first = tuple.0;
    let _second = tuple.1;
}

// ========== SC-10: a.b.c.d (nested field access) ==========
struct Outer {
    inner: Inner,
}
struct Inner {
    value: Leaf,
}
struct Leaf {
    data: i32,
}

fn access_nested_fields() {
    let outer = Outer {
        inner: Inner {
            value: Leaf { data: 42 },
        },
    };
    let _data = outer.inner.value.data;
}

// ========== SC-11: p.method() (method call, NOT field access) ==========
struct Processor;

impl Processor {
    fn process(&self) -> i32 {
        42
    }
}

fn call_method() {
    let p = Processor;
    let _result = p.process();  // This is a method call, NOT field access
}

// ========== SC-12: Long operand >128 chars (hash truncation) ==========
struct VeryLongStructNameThatExceedsNormalLengthLimitsForTestingPurposesInOurCodebaseAnalysisSystem {
    field: i32,
}

fn access_long_operand() {
    let long_var = VeryLongStructNameThatExceedsNormalLengthLimitsForTestingPurposesInOurCodebaseAnalysisSystem { field: 1 };
    let _f = long_var.field;
}

// ========== SC-13: std::io::Read import (stdlib) ==========
use std::io::Read;
use std::collections::HashMap;
use core::mem::swap;

// ========== SC-14: serde::Deserialize import (external) ==========
use serde::Deserialize;
use tokio::runtime::Runtime;

// ========== SC-15: receiver.call() (method call) ==========
struct Receiver;

impl Receiver {
    fn call(&self) {
        println!("called");
    }
}

fn test_method_call() {
    let receiver = Receiver;
    receiver.call();  // method call expression
}

// ========== SC-16: Call in unsafe {} block ==========
fn unsafe_helper() {}

fn test_unsafe_block() {
    unsafe {
        unsafe_helper();  // call inside unsafe block
    }
}

// Additional: unsafe fn (is_unsafe = true, but NOT is_unsafe_block)
unsafe fn unsafe_function() {
    unsafe_helper();  // is_unsafe = true, is_unsafe_block = false
}
