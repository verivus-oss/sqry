// Rust visibility test fixture

pub fn public_function() -> String {
    String::from("public")
}

fn private_function() -> String {
    String::from("private")
}
