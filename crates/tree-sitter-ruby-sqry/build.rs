use std::path::PathBuf;

fn main() {
    let dir: PathBuf = ["grammar-src"].iter().collect();

    // Mirrors the upstream tree-sitter-ruby build script: the generated parser
    // and the external scanner share the bundled runtime headers under
    // grammar-src/tree_sitter/, and the scanner is written against C11.
    let mut c_config = cc::Build::new();
    c_config
        .std("c11")
        .include(&dir)
        .flag_if_supported("-Wno-unused-value");

    #[cfg(target_env = "msvc")]
    c_config.flag("-utf-8");

    let parser_path = dir.join("parser.c");
    c_config.file(&parser_path);
    println!("cargo:rerun-if-changed={}", parser_path.display());

    let scanner_path = dir.join("scanner.c");
    c_config.file(&scanner_path);
    println!("cargo:rerun-if-changed={}", scanner_path.display());

    c_config.compile("parser");
}
