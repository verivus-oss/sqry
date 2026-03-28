// Minimal Rust file for polyglot test fixture
// This file provides a simple function to verify Rust node extraction

pub fn hello_rust() -> String {
    String::from("Hello from Rust")
}

pub struct Config {
    pub name: String,
    pub version: u32,
}

impl Config {
    pub fn new(name: String, version: u32) -> Self {
        Config { name, version }
    }
}
