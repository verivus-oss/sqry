//! Pipeline/aggregation result types for post-query analysis.
//!
//! The pipeline syntax `query | stage1 | stage2` enables post-query analysis
//! via aggregation stages like `count`, `group_by`, `top`, and `stats`.
//!
//! # Stages
//!
//! - `count` — Total number of matched symbols
//! - `group_by <field>` — Group matches by field value with counts
//! - `top <N> <field>` — Top N groups by count
//! - `stats` — Comprehensive summary (count, by-kind, by-lang, by-visibility)

use std::fmt;

/// Result of a pipeline aggregation stage.
pub enum AggregationResult {
    /// Total count of matched symbols.
    Count(CountResult),
    /// Symbols grouped by a field value.
    GroupBy(GroupByResult),
    /// Top N groups by count.
    Top(TopResult),
    /// Comprehensive statistics.
    Stats(StatsResult),
}

/// Count of matched symbols.
pub struct CountResult {
    /// Total number of matched symbols.
    pub total: usize,
}

/// Symbols grouped by a field value, sorted descending by count.
pub struct GroupByResult {
    /// The field used for grouping.
    pub field: String,
    /// Groups as `(value, count)` pairs, sorted descending by count.
    pub groups: Vec<(String, usize)>,
}

/// Top N groups by count.
pub struct TopResult {
    /// The field used for grouping.
    pub field: String,
    /// The requested number of top entries.
    pub n: usize,
    /// Top entries as `(value, count)` pairs, sorted descending by count.
    pub entries: Vec<(String, usize)>,
}

/// Comprehensive statistics about matched symbols.
pub struct StatsResult {
    /// Total number of matched symbols.
    pub total: usize,
    /// Breakdown by node kind (e.g., function, class, method).
    pub by_kind: Vec<(String, usize)>,
    /// Breakdown by language.
    pub by_lang: Vec<(String, usize)>,
    /// Breakdown by visibility.
    pub by_visibility: Vec<(String, usize)>,
}

impl fmt::Display for AggregationResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Count(r) => write!(f, "{r}"),
            Self::GroupBy(r) => write!(f, "{r}"),
            Self::Top(r) => write!(f, "{r}"),
            Self::Stats(r) => write!(f, "{r}"),
        }
    }
}

impl fmt::Display for CountResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.total)
    }
}

impl fmt::Display for GroupByResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (value, count) in &self.groups {
            writeln!(f, "{value}: {count}")?;
        }
        Ok(())
    }
}

impl fmt::Display for TopResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (value, count) in &self.entries {
            writeln!(f, "{value}: {count}")?;
        }
        Ok(())
    }
}

impl fmt::Display for StatsResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Total: {}", self.total)?;
        writeln!(f, "\nBy kind:")?;
        for (kind, count) in &self.by_kind {
            writeln!(f, "  {kind}: {count}")?;
        }
        writeln!(f, "\nBy language:")?;
        for (lang, count) in &self.by_lang {
            writeln!(f, "  {lang}: {count}")?;
        }
        writeln!(f, "\nBy visibility:")?;
        for (vis, count) in &self.by_visibility {
            writeln!(f, "  {vis}: {count}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_count_result_display() {
        let result = CountResult { total: 42 };
        assert_eq!(format!("{result}"), "42");
    }

    #[test]
    fn test_group_by_result_display() {
        let result = GroupByResult {
            field: "lang".to_string(),
            groups: vec![("rust".to_string(), 10), ("python".to_string(), 5)],
        };
        let output = format!("{result}");
        assert!(output.contains("rust: 10"));
        assert!(output.contains("python: 5"));
    }

    #[test]
    fn test_top_result_display() {
        let result = TopResult {
            field: "lang".to_string(),
            n: 2,
            entries: vec![("rust".to_string(), 10), ("python".to_string(), 5)],
        };
        let output = format!("{result}");
        assert!(output.contains("rust: 10"));
        assert!(output.contains("python: 5"));
    }

    #[test]
    fn test_stats_result_display() {
        let result = StatsResult {
            total: 15,
            by_kind: vec![("function".to_string(), 10)],
            by_lang: vec![("rust".to_string(), 15)],
            by_visibility: vec![("public".to_string(), 8)],
        };
        let output = format!("{result}");
        assert!(output.contains("Total: 15"));
        assert!(output.contains("function: 10"));
        assert!(output.contains("rust: 15"));
        assert!(output.contains("public: 8"));
    }
}
