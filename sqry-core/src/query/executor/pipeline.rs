//! Pipeline stage execution for post-query aggregation.
//!
//! Executes pipeline stages (`count`, `group_by`, `top`, `stats`) on query results.

use crate::query::pipeline::{
    AggregationResult, CountResult, GroupByResult, StatsResult, TopResult,
};
use crate::query::results::QueryResults;
use crate::query::types::PipelineStage;
use std::collections::HashMap;

/// Execute a single pipeline stage on query results.
#[must_use]
pub fn execute_pipeline_stage(results: &QueryResults, stage: &PipelineStage) -> AggregationResult {
    match stage {
        PipelineStage::Count => AggregationResult::Count(CountResult {
            total: results.len(),
        }),
        PipelineStage::GroupBy { field } => execute_group_by(results, field.as_str()),
        PipelineStage::Top { n, field } => execute_top(results, *n, field.as_str()),
        PipelineStage::Stats => execute_stats(results),
    }
}

fn execute_group_by(results: &QueryResults, field: &str) -> AggregationResult {
    let mut groups: HashMap<String, usize> = HashMap::new();

    for m in results.iter() {
        let value = extract_field_value_for_match(&m, field);
        *groups.entry(value).or_insert(0) += 1;
    }

    let mut sorted: Vec<(String, usize)> = groups.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    AggregationResult::GroupBy(GroupByResult {
        field: field.to_string(),
        groups: sorted,
    })
}

fn execute_top(results: &QueryResults, n: usize, field: &str) -> AggregationResult {
    let mut groups: HashMap<String, usize> = HashMap::new();

    for m in results.iter() {
        let value = extract_field_value_for_match(&m, field);
        *groups.entry(value).or_insert(0) += 1;
    }

    let mut sorted: Vec<(String, usize)> = groups.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    sorted.truncate(n);

    AggregationResult::Top(TopResult {
        field: field.to_string(),
        n,
        entries: sorted,
    })
}

fn execute_stats(results: &QueryResults) -> AggregationResult {
    let mut by_kind: HashMap<String, usize> = HashMap::new();
    let mut by_lang: HashMap<String, usize> = HashMap::new();
    let mut by_visibility: HashMap<String, usize> = HashMap::new();

    for m in results.iter() {
        *by_kind.entry(m.kind().as_str().to_string()).or_insert(0) += 1;

        let lang = m
            .language()
            .map_or_else(|| "unknown".to_string(), |l| l.to_string());
        *by_lang.entry(lang).or_insert(0) += 1;

        let vis = m
            .visibility()
            .map_or_else(|| "unspecified".to_string(), |v| v.to_string());
        *by_visibility.entry(vis).or_insert(0) += 1;
    }

    let sort_desc = |map: HashMap<String, usize>| -> Vec<(String, usize)> {
        let mut v: Vec<_> = map.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v
    };

    AggregationResult::Stats(StatsResult {
        total: results.len(),
        by_kind: sort_desc(by_kind),
        by_lang: sort_desc(by_lang),
        by_visibility: sort_desc(by_visibility),
    })
}

/// Extract a field value from a query match for aggregation purposes.
fn extract_field_value_for_match(m: &crate::query::results::QueryMatch<'_>, field: &str) -> String {
    match field {
        "kind" => m.kind().as_str().to_string(),
        "lang" | "language" => m
            .language()
            .map_or_else(|| "unknown".to_string(), |l| l.to_string()),
        "name" => m
            .name()
            .map_or_else(|| "<anonymous>".to_string(), |n| n.to_string()),
        "visibility" => m
            .visibility()
            .map_or_else(|| "unspecified".to_string(), |v| v.to_string()),
        "path" => m
            .relative_path()
            .map_or_else(|| "<unknown>".to_string(), |p| p.display().to_string()),
        "async" => m.is_async().to_string(),
        "static" => m.is_static().to_string(),
        _ => "<unsupported>".to_string(),
    }
}
