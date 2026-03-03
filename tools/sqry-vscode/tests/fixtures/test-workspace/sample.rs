// Sample Rust file for testing search functionality

pub struct ErrorHandler {
    pub error_count: usize,
}

impl ErrorHandler {
    pub fn new() -> Self {
        Self { error_count: 0 }
    }

    pub fn handle_error(&mut self, error: &str) -> Result<(), String> {
        self.error_count += 1;
        println!("Error: {}", error);
        Ok(())
    }

    pub fn get_error_count(&self) -> usize {
        self.error_count
    }
}

pub fn process_data(input: &str) -> Result<String, String> {
    if input.is_empty() {
        return Err("Empty input".to_string());
    }
    Ok(input.to_uppercase())
}

pub async fn async_operation() -> Result<i32, String> {
    Ok(42)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_handler() {
        let mut handler = ErrorHandler::new();
        assert_eq!(handler.get_error_count(), 0);

        handler.handle_error("test error").unwrap();
        assert_eq!(handler.get_error_count(), 1);
    }
}
