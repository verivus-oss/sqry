use anyhow::Result;
use globset::{Glob, GlobSet, GlobSetBuilder};

/// Filter helper for repository scoping predicates (`repo:`).
#[derive(Debug, Clone, Default)]
pub struct RepoFilter {
    matcher: Option<GlobSet>,
    patterns: Vec<String>,
}

impl RepoFilter {
    /// Construct a filter from the provided glob patterns.
    ///
    /// # Errors
    ///
    /// Returns [`anyhow::Error`] when glob compilation fails.
    pub fn new(patterns: Vec<String>) -> Result<Self> {
        if patterns.is_empty() {
            return Ok(Self {
                matcher: None,
                patterns,
            });
        }

        let mut builder = GlobSetBuilder::new();
        for pattern in &patterns {
            builder.add(Glob::new(pattern)?);
        }

        let matcher = builder.build()?;

        Ok(Self {
            matcher: Some(matcher),
            patterns,
        })
    }

    /// True when the filter matches all repositories (no predicates supplied).
    #[must_use]
    pub fn is_universal(&self) -> bool {
        self.matcher.is_none()
    }

    /// Returns the original glob patterns backing this filter.
    #[must_use]
    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }

    /// Returns whether the filter matches `repo_name`.
    #[must_use]
    pub fn matches(&self, repo_name: &str) -> bool {
        match &self.matcher {
            Some(matcher) => matcher.is_match(repo_name),
            None => true,
        }
    }
}
