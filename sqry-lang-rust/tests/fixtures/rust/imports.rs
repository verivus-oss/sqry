// Test fixture: use declarations with aliases
// Tests: HashMap, PathBuf as Path

use std::collections::HashMap as Map;
use std::path::PathBuf as Path;
use std::fs::File as FileHandle;

fn create_config() -> Map<String, String> {
    let mut config = Map::new();
    config.insert("host".to_string(), "localhost".to_string());
    config.insert("port".to_string(), "8080".to_string());
    config
}

fn get_config_path() -> Path {
    let mut path = Path::new();
    path.push("/etc");
    path.push("config.toml");
    path
}

fn open_file(path: &Path) -> std::io::Result<FileHandle> {
    FileHandle::open(path)
}

fn main() {
    let config = create_config();
    println!("Config entries: {}", config.len());

    let path = get_config_path();
    println!("Config path: {}", path.display());

    // Attempt to open file (may fail, which is OK for this test)
    let _ = open_file(&path);
}
