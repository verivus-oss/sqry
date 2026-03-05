// Fixture 1: Minimal (~50 LOC)
// Tests basic cross-file call tracking

pub mod utils {
    pub fn helper() -> String {
        String::from("helper")
    }

    pub fn process(input: &str) -> Result<String, String> {
        if input.is_empty() {
            Err("empty".to_string())
        } else {
            Ok(input.to_uppercase())
        }
    }
}

pub mod service {
    use crate::utils;

    pub fn fetch() -> Result<String, String> {
        let data = utils::helper();
        utils::process(&data)
    }

    pub fn save(value: String) -> bool {
        !value.is_empty()
    }
}

pub mod api {
    use crate::service;

    pub fn handle_request() -> Result<String, String> {
        service::fetch()
    }

    pub fn handle_save() -> bool {
        service::save("data".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fetch() {
        assert!(api::handle_request().is_ok());
    }
}
