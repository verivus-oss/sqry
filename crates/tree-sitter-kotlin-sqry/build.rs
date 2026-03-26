use std::path::PathBuf;

fn main() {
    let dir: PathBuf = ["grammar-src"].iter().collect();

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

    let scanner_c = dir.join("scanner.c");
    if scanner_c.exists() {
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
