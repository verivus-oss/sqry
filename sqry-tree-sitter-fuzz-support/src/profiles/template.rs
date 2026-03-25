//! Template language family profiles (3 languages).
//!
//! These languages use nested HTML-like tags for structuring.

use super::LanguageProfile;

/// HTML language profile.
pub struct HtmlProfile;

impl LanguageProfile for HtmlProfile {
    fn language_name(&self) -> &'static str {
        "html"
    }

    fn generate_deeply_nested(&self, depth: usize) -> Vec<u8> {
        let mut result = Vec::new();

        // Generate deeply nested <div> tags
        for _ in 0..depth {
            result.extend_from_slice(b"<div>");
        }

        result.extend_from_slice(b"content");

        for _ in 0..depth {
            result.extend_from_slice(b"</div>");
        }

        result
    }

    fn minimal_valid(&self) -> &'static str {
        "<div></div>"
    }
}

/// Vue language profile.
pub struct VueProfile;

impl LanguageProfile for VueProfile {
    fn language_name(&self) -> &'static str {
        "vue"
    }

    fn generate_deeply_nested(&self, depth: usize) -> Vec<u8> {
        let mut result = Vec::new();

        // Vue uses template tags with nested divs
        result.extend_from_slice(b"<template>");

        for _ in 0..depth {
            result.extend_from_slice(b"<div>");
        }

        result.extend_from_slice(b"content");

        for _ in 0..depth {
            result.extend_from_slice(b"</div>");
        }

        result.extend_from_slice(b"</template>");

        result
    }

    fn minimal_valid(&self) -> &'static str {
        "<template><div></div></template>"
    }
}

/// Svelte language profile.
pub struct SvelteProfile;

impl LanguageProfile for SvelteProfile {
    fn language_name(&self) -> &'static str {
        "svelte"
    }

    fn generate_deeply_nested(&self, depth: usize) -> Vec<u8> {
        let mut result = Vec::new();

        // Svelte uses nested HTML tags
        for _ in 0..depth {
            result.extend_from_slice(b"<div>");
        }

        result.extend_from_slice(b"content");

        for _ in 0..depth {
            result.extend_from_slice(b"</div>");
        }

        result
    }

    fn minimal_valid(&self) -> &'static str {
        "<div></div>"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_html_profile() {
        let profile = HtmlProfile;
        assert_eq!(profile.language_name(), "html");
        assert_eq!(profile.minimal_valid(), "<div></div>");

        let nested = profile.generate_deeply_nested(3);
        let nested_str = String::from_utf8(nested).unwrap();

        assert_eq!(nested_str.matches("<div>").count(), 3);
        assert_eq!(nested_str.matches("</div>").count(), 3);
        assert!(nested_str.contains("content"));
    }

    #[test]
    fn test_vue_profile() {
        let profile = VueProfile;
        assert_eq!(profile.language_name(), "vue");

        let nested = profile.generate_deeply_nested(500);
        let nested_str = String::from_utf8(nested).unwrap();

        assert!(nested_str.starts_with("<template>"));
        assert!(nested_str.ends_with("</template>"));
        assert_eq!(nested_str.matches("<div>").count(), 500);
        assert_eq!(nested_str.matches("</div>").count(), 500);
    }

    #[test]
    fn test_svelte_profile() {
        let profile = SvelteProfile;
        assert_eq!(profile.language_name(), "svelte");

        let nested = profile.generate_deeply_nested(10);
        let nested_str = String::from_utf8(nested).unwrap();

        assert_eq!(nested_str.matches("<div>").count(), 10);
        assert_eq!(nested_str.matches("</div>").count(), 10);
    }

    #[test]
    fn test_all_template_profiles() {
        let profiles: Vec<Box<dyn LanguageProfile>> = vec![
            Box::new(HtmlProfile),
            Box::new(VueProfile),
            Box::new(SvelteProfile),
        ];

        assert_eq!(profiles.len(), 3, "Should have 3 Template profiles");

        for profile in profiles {
            let nested = profile.generate_deeply_nested(10);
            assert!(!nested.is_empty());
            assert!(!profile.minimal_valid().is_empty());

            // All template languages should have balanced tags
            let nested_str = String::from_utf8(nested).unwrap();
            let open_tags = nested_str.matches("<div>").count();
            let close_tags = nested_str.matches("</div>").count();
            assert_eq!(open_tags, close_tags, "Tags should be balanced");
        }
    }
}
