// Test fixture: Basic function calls
// Tests: main→greet, process_data→fetch→transform

fn greet(name: &str) -> String {
    format!("Hello, {}!", name)
}

fn fetch(id: u32) -> Option<String> {
    if id > 0 {
        Some(format!("Data-{}", id))
    } else {
        None
    }
}

fn transform(data: String) -> String {
    data.to_uppercase()
}

fn process_data(id: u32) -> Option<String> {
    let raw = fetch(id)?;
    let processed = transform(raw);
    Some(processed)
}

fn main() {
    let message = greet("World");
    println!("{}", message);

    if let Some(result) = process_data(42) {
        println!("Processed: {}", result);
    }
}
