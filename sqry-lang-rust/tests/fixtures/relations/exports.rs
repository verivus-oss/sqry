//! Test fixture for Rust export patterns
//!
//! Tests visibility-based export extraction:
//! - pub items should be exported
//! - pub(crate) items behavior (document actual current behavior)
//! - private items should NOT be exported

// Public exports (should be extracted)
pub fn public_function() {
    println!("Public function");
}

pub struct PublicStruct {
    pub field: i32,
}

pub enum PublicEnum {
    Variant1,
    Variant2,
}

pub trait PublicTrait {
    fn trait_method(&self);
}

// Crate-visible items (document current behavior)
pub(crate) fn crate_function() {
    println!("Crate function");
}

pub(crate) struct CrateStruct {
    field: i32,
}

// Private items (should NOT be exported)
fn private_function() {
    println!("Private function");
}

struct PrivateStruct {
    field: i32,
}

enum PrivateEnum {
    A,
    B,
}

// Mixed visibility
pub struct MixedStruct {
    pub public_field: i32,
    private_field: i32,
}

impl PublicStruct {
    pub fn new(field: i32) -> Self {
        PublicStruct { field }
    }

    pub fn public_method(&self) {
        println!("Public method");
    }

    fn private_method(&self) {
        println!("Private method");
    }
}

// Module with exports
pub mod public_module {
    pub fn module_function() {
        println!("Module function");
    }
}

mod private_module {
    pub fn function_in_private_module() {
        // This is pub but in a private module
        println!("Function in private module");
    }
}

fn main() {
    public_function();
}
