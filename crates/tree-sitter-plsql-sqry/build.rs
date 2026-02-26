use std::path::PathBuf;

fn main() {
    let dir: PathBuf = ["../../vendor", "tree-sitter-plsql", "src"]
        .iter()
        .collect();

    cc::Build::new()
        .include(&dir)
        .file(dir.join("parser.c"))
        .compile("tree-sitter-plsql");
}
