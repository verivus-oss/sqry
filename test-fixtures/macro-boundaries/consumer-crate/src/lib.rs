use test_proc_macro::{MyDerive, WithHelper};

#[derive(MyDerive)]
struct Derived {}

/// Exercises the `#[proc_macro_derive(WithHelper, attributes(helper))]` path.
/// The `#[helper(...)]` attribute is an inert helper attribute recognized only
/// inside `#[derive(WithHelper)]` — it is NOT a proc-macro attribute itself.
#[derive(WithHelper)]
struct WithHelperStruct {
    #[helper(rename = "other_name")]
    field: i32,
}

#[test_proc_macro::my_attribute]
fn attributed() {}

test_proc_macro::my_function_like!();
