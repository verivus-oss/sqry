// Test fixture: Wildcard imports
// Tests: use std::prelude::*, use std::collections::*

use std::collections::*;

fn create_structures() {
    let mut map = HashMap::new();
    map.insert("key1", "value1");
    map.insert("key2", "value2");

    let mut set = HashSet::new();
    set.insert(1);
    set.insert(2);
    set.insert(3);

    let mut vec = VecDeque::new();
    vec.push_back(10);
    vec.push_back(20);

    println!("Map size: {}", map.len());
    println!("Set size: {}", set.len());
    println!("VecDeque size: {}", vec.len());
}

use std::io::*;

fn perform_io() -> Result<()> {
    let mut buffer = Vec::new();
    let cursor = Cursor::new(&mut buffer);
    println!("Created I/O structures");
    Ok(())
}

fn main() {
    create_structures();
    let _ = perform_io();
}
