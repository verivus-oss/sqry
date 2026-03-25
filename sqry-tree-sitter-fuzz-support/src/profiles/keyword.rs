//! Keyword-based language family profiles (5 languages).
//!
//! These languages use BEGIN/END or similar keyword pairs for block structuring.

use super::LanguageProfile;

/// SQL language profile.
pub struct SqlProfile;

impl LanguageProfile for SqlProfile {
    fn language_name(&self) -> &'static str {
        "sql"
    }

    fn generate_deeply_nested(&self, depth: usize) -> Vec<u8> {
        let mut result = Vec::new();

        // Generate deeply nested BEGIN/END blocks
        for _ in 0..depth {
            result.extend_from_slice(b"BEGIN ");
        }

        result.extend_from_slice(b"SELECT 1; ");

        for _ in 0..depth {
            result.extend_from_slice(b"END; ");
        }

        result
    }

    fn minimal_valid(&self) -> &'static str {
        "SELECT 1;"
    }
}

/// Shell (Bash) language profile.
pub struct ShellProfile;

impl LanguageProfile for ShellProfile {
    fn language_name(&self) -> &'static str {
        "shell"
    }

    fn generate_deeply_nested(&self, depth: usize) -> Vec<u8> {
        let mut result = Vec::new();

        // Generate deeply nested if statements
        for _ in 0..depth {
            result.extend_from_slice(b"if true; then ");
        }

        result.extend_from_slice(b"echo ok; ");

        for _ in 0..depth {
            result.extend_from_slice(b"fi; ");
        }

        result
    }

    fn minimal_valid(&self) -> &'static str {
        "echo ok"
    }
}

/// Lua language profile.
pub struct LuaProfile;

impl LanguageProfile for LuaProfile {
    fn language_name(&self) -> &'static str {
        "lua"
    }

    fn generate_deeply_nested(&self, depth: usize) -> Vec<u8> {
        let mut result = Vec::new();

        // Generate deeply nested do/end blocks
        for _ in 0..depth {
            result.extend_from_slice(b"do ");
        }

        result.extend_from_slice(b"local x = 1 ");

        for _ in 0..depth {
            result.extend_from_slice(b"end ");
        }

        result
    }

    fn minimal_valid(&self) -> &'static str {
        "return 1"
    }
}

/// Groovy language profile.
pub struct GroovyProfile;

impl LanguageProfile for GroovyProfile {
    fn language_name(&self) -> &'static str {
        "groovy"
    }

    fn generate_deeply_nested(&self, depth: usize) -> Vec<u8> {
        let mut result = Vec::new();

        // Groovy uses curly braces but also supports keyword-based closures
        // We'll use nested closures for deep nesting
        for _ in 0..depth {
            result.extend_from_slice(b"{ ");
        }

        result.extend_from_slice(b"println 'ok' ");

        for _ in 0..depth {
            result.extend_from_slice(b"} ");
        }

        result
    }

    fn minimal_valid(&self) -> &'static str {
        "println 'ok'"
    }
}

/// PL/SQL language profile.
pub struct PlsqlProfile;

impl LanguageProfile for PlsqlProfile {
    fn language_name(&self) -> &'static str {
        "plsql"
    }

    fn generate_deeply_nested(&self, depth: usize) -> Vec<u8> {
        let mut result = Vec::new();

        // Generate deeply nested BEGIN/END blocks
        for _ in 0..depth {
            result.extend_from_slice(b"BEGIN ");
        }

        result.extend_from_slice(b"NULL; ");

        for _ in 0..depth {
            result.extend_from_slice(b"END; ");
        }

        result
    }

    fn minimal_valid(&self) -> &'static str {
        "BEGIN NULL; END;"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sql_profile() {
        let profile = SqlProfile;
        assert_eq!(profile.language_name(), "sql");
        assert_eq!(profile.minimal_valid(), "SELECT 1;");

        let nested = profile.generate_deeply_nested(3);
        let nested_str = String::from_utf8(nested).unwrap();

        assert_eq!(nested_str.matches("BEGIN").count(), 3);
        assert_eq!(nested_str.matches("END").count(), 3);
        assert!(nested_str.contains("SELECT 1"));
    }

    #[test]
    fn test_shell_profile() {
        let profile = ShellProfile;
        assert_eq!(profile.language_name(), "shell");

        let nested = profile.generate_deeply_nested(500);
        let nested_str = String::from_utf8(nested).unwrap();

        assert_eq!(nested_str.matches("if true").count(), 500);
        assert_eq!(nested_str.matches("fi").count(), 500);
    }

    #[test]
    fn test_lua_profile() {
        let profile = LuaProfile;
        assert_eq!(profile.language_name(), "lua");

        let nested = profile.generate_deeply_nested(10);
        let nested_str = String::from_utf8(nested).unwrap();

        assert_eq!(nested_str.matches("do").count(), 10);
        assert_eq!(nested_str.matches("end").count(), 10);
    }

    #[test]
    fn test_groovy_profile() {
        let profile = GroovyProfile;
        assert_eq!(profile.language_name(), "groovy");

        let nested = profile.generate_deeply_nested(5);
        let nested_str = String::from_utf8(nested).unwrap();

        assert_eq!(nested_str.matches('{').count(), 5);
        assert_eq!(nested_str.matches('}').count(), 5);
    }

    #[test]
    fn test_plsql_profile() {
        let profile = PlsqlProfile;
        assert_eq!(profile.language_name(), "plsql");
        assert_eq!(profile.minimal_valid(), "BEGIN NULL; END;");

        let nested = profile.generate_deeply_nested(100);
        let nested_str = String::from_utf8(nested).unwrap();

        assert_eq!(nested_str.matches("BEGIN").count(), 100);
        assert_eq!(nested_str.matches("END").count(), 100);
        assert!(nested_str.contains("NULL"));
    }

    #[test]
    fn test_all_keyword_profiles() {
        let profiles: Vec<Box<dyn LanguageProfile>> = vec![
            Box::new(SqlProfile),
            Box::new(ShellProfile),
            Box::new(LuaProfile),
            Box::new(GroovyProfile),
            Box::new(PlsqlProfile),
        ];

        assert_eq!(profiles.len(), 5, "Should have 5 Keyword profiles");

        for profile in profiles {
            let nested = profile.generate_deeply_nested(10);
            assert!(!nested.is_empty());
            assert!(!profile.minimal_valid().is_empty());
        }
    }
}
