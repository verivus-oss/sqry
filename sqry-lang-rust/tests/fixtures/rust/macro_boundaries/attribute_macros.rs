// Well-known proc-macro attributes
#[tokio::main]
async fn main() {}

#[tokio::test]
async fn test_something() {}

#[tracing::instrument]
fn traced_function() {}

// Built-in attributes that should be skipped
#[inline]
fn inlined() {}

#[must_use]
fn important() -> i32 {
    42
}

#[deprecated]
fn old_function() {}

// Derive attributes (handled separately, not by attribute_macros analyzer)
#[derive(Debug, Clone)]
struct MyStruct {
    field: i32,
}

// Unknown attributes (should be recorded as unresolved)
#[my_custom_attr]
fn custom_annotated() {}

// Multiple attributes on one item
#[tokio::test]
#[tracing::instrument]
async fn multi_attr_test() {}

// Inner attributes (should be skipped by attribute analyzer)
// #![allow(unused)]

// cfg attributes (handled by cfg_analysis, not attribute_macros)
#[cfg(test)]
mod tests {}
