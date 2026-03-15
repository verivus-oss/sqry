use std::path::PathBuf;

fn main() {
    let dir: PathBuf = ["..", "..", "vendor", "tree-sitter-groovy", "src"]
        .iter()
        .collect();

    // Compile parser.c as C
    let mut c_config = cc::Build::new();
    c_config.include(&dir);
    c_config
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-unused-but-set-variable")
        .flag_if_supported("-Wno-trigraphs")
        .flag_if_supported("-std=c99");

    let parser_path = dir.join("parser.c");
    c_config.file(&parser_path);
    c_config.compile("parser");
    println!("cargo:rerun-if-changed={}", parser_path.display());

    // Compile scanner.c or scanner.cc separately if it exists
    // Note: Avoid canonicalize() on Windows as it produces UNC paths (\\?\...)
    // that MSVC doesn't handle properly. Use the relative path directly.
    let scanner_cc = dir.join("scanner.cc");
    let scanner_c = dir.join("scanner.c");

    if scanner_cc.exists() {
        let mut cpp_config = cc::Build::new();
        cpp_config.include(&dir);
        cpp_config
            .cpp(true)
            .flag_if_supported("-Wno-unused-parameter")
            .flag_if_supported("-Wno-unused-but-set-variable");
        cpp_config.file(&scanner_cc);
        cpp_config.compile("scanner");
        println!("cargo:rerun-if-changed={}", scanner_cc.display());
    } else if scanner_c.exists() {
        let mut c_scanner_config = cc::Build::new();
        c_scanner_config.include(&dir);
        c_scanner_config
            .flag_if_supported("-Wno-unused-parameter")
            .flag_if_supported("-std=c99");
        c_scanner_config.file(&scanner_c);
        c_scanner_config.compile("scanner");
        println!("cargo:rerun-if-changed={}", scanner_c.display());
    }
}
