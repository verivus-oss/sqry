use std::path::PathBuf;

fn main() {
    let dir: PathBuf = ["grammar-src"].iter().collect();

    // The upstream tree-sitter-perl v1.2.1 external scanner shares its bundled
    // runtime headers (grammar-src/tree_sitter/*.h) and scanner helpers
    // (grammar-src/tsp_*.h) with the generated parser, so parser.c and scanner.c
    // are compiled together into a single object, matching upstream's own
    // build.rs. Forcing an older -std here breaks the v1.2.1 scanner (it relies
    // on features its headers assume), so we only pass warning-suppression flags.
    let mut c_config = cc::Build::new();
    c_config.include(&dir);
    c_config
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-unused-but-set-variable")
        .flag_if_supported("-Wno-empty-body")
        .flag_if_supported("-Wno-trigraphs");

    let parser_path = dir.join("parser.c");
    c_config.file(&parser_path);
    println!("cargo:rerun-if-changed={}", parser_path.display());

    let scanner_c = dir.join("scanner.c");
    if scanner_c.exists() {
        c_config.file(&scanner_c);
        println!("cargo:rerun-if-changed={}", scanner_c.display());
    }

    c_config.compile("parser");
}
