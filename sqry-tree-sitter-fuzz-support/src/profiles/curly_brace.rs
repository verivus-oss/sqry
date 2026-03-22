//! `CurlyBrace` language family profiles (22 languages).
//!
//! These languages use curly braces `{}` for block structuring, making them
//! susceptible to deeply nested block constructs.

use super::LanguageProfile;

/// Helper function to generate deeply nested curly-brace blocks.
///
/// Creates a pattern like: `{ { { ... } } }`
fn generate_nested_blocks(depth: usize) -> Vec<u8> {
    let mut result = Vec::with_capacity(depth * 3); // Approximate: "{ " per level + "}" per level

    // Opening braces
    for _ in 0..depth {
        result.extend_from_slice(b"{ ");
    }

    // Closing braces
    for _ in 0..depth {
        result.extend_from_slice(b"} ");
    }

    result
}

/// Macro to define `CurlyBrace` language profiles with minimal boilerplate.
macro_rules! curly_brace_profile {
    ($name:ident, $lang_str:expr, $minimal:expr) => {
        pub struct $name;

        impl LanguageProfile for $name {
            fn language_name(&self) -> &'static str {
                $lang_str
            }

            fn generate_deeply_nested(&self, depth: usize) -> Vec<u8> {
                generate_nested_blocks(depth)
            }

            fn minimal_valid(&self) -> &'static str {
                $minimal
            }
        }
    };
}

// CurlyBrace family: 22 languages

curly_brace_profile!(RustProfile, "rust", "fn main() {}");
curly_brace_profile!(CProfile, "c", "int main() { return 0; }");
curly_brace_profile!(CppProfile, "cpp", "int main() { return 0; }");
curly_brace_profile!(JavaProfile, "java", "class Main { void m() {} }");
curly_brace_profile!(JavaScriptProfile, "javascript", "function f() {}");
curly_brace_profile!(TypeScriptProfile, "typescript", "function f() {}");
curly_brace_profile!(GoProfile, "go", "func main() {}");
curly_brace_profile!(CSharpProfile, "csharp", "class C { void M() {} }");
curly_brace_profile!(KotlinProfile, "kotlin", "fun main() {}");
curly_brace_profile!(SwiftProfile, "swift", "func main() {}");
curly_brace_profile!(ScalaProfile, "scala", "object Main { def main() {} }");
curly_brace_profile!(DartProfile, "dart", "void main() {}");
curly_brace_profile!(HaskellProfile, "haskell", "main = return ()");
curly_brace_profile!(ZigProfile, "zig", "pub fn main() void {}");
curly_brace_profile!(ApexProfile, "apex", "public class C { void m() {} }");
curly_brace_profile!(TerraformProfile, "terraform", "resource \"null\" \"n\" {}");
curly_brace_profile!(PuppetProfile, "puppet", "class example {}");
curly_brace_profile!(XanaduProfile, "xanadu", "class Main {}");
curly_brace_profile!(CssProfile, "css", "body {}");
curly_brace_profile!(RProfile, "r", "f <- function() {}");
curly_brace_profile!(ElixirProfile, "elixir", "defmodule M do end");
curly_brace_profile!(PerlProfile, "perl", "sub main {}");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rust_profile() {
        let profile = RustProfile;
        assert_eq!(profile.language_name(), "rust");
        assert_eq!(profile.minimal_valid(), "fn main() {}");

        let nested = profile.generate_deeply_nested(3);
        let nested_str = String::from_utf8(nested).unwrap();
        assert_eq!(nested_str, "{ { { } } } ");
    }

    #[test]
    fn test_deeply_nested_structure() {
        let profile = JavaProfile;
        let nested = profile.generate_deeply_nested(500);
        let nested_str = String::from_utf8(nested).unwrap();

        // Should have 500 opening braces and 500 closing braces
        assert_eq!(nested_str.matches('{').count(), 500);
        assert_eq!(nested_str.matches('}').count(), 500);
    }

    #[test]
    fn test_all_curly_brace_profiles() {
        let profiles: Vec<Box<dyn LanguageProfile>> = vec![
            Box::new(RustProfile),
            Box::new(CProfile),
            Box::new(CppProfile),
            Box::new(JavaProfile),
            Box::new(JavaScriptProfile),
            Box::new(TypeScriptProfile),
            Box::new(GoProfile),
            Box::new(CSharpProfile),
            Box::new(KotlinProfile),
            Box::new(SwiftProfile),
            Box::new(ScalaProfile),
            Box::new(DartProfile),
            Box::new(HaskellProfile),
            Box::new(ZigProfile),
            Box::new(ApexProfile),
            Box::new(TerraformProfile),
            Box::new(PuppetProfile),
            Box::new(XanaduProfile),
            Box::new(CssProfile),
            Box::new(RProfile),
            Box::new(ElixirProfile),
            Box::new(PerlProfile),
        ];

        assert_eq!(profiles.len(), 22, "Should have 22 CurlyBrace profiles");

        for profile in profiles {
            // Verify each profile generates valid nested blocks
            let nested = profile.generate_deeply_nested(10);
            assert!(!nested.is_empty());

            // Verify minimal valid syntax is non-empty
            assert!(!profile.minimal_valid().is_empty());
        }
    }
}
