//! Query execution plan types for --explain output

use serde::{Deserialize, Serialize};

/// Query execution plan with optimization details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryPlan {
    /// Schema version for forward compatibility
    pub schema_version: u32,
    /// Original query string
    pub original_query: String,
    /// Optimized query (with predicate reordering)
    pub optimized_query: String,
    /// Execution steps with timing
    pub steps: Vec<ExecutionStep>,
    /// Total execution time in milliseconds
    pub execution_time_ms: u64,
    /// Whether index was used
    pub used_index: bool,
    /// Cache hit/miss status
    pub cache_status: CacheStatus,
}

/// Single step in query execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionStep {
    /// Step number (1-indexed)
    pub step_num: usize,
    /// Operation description
    pub operation: String,
    /// Number of results after this step
    pub result_count: usize,
    /// Time taken for this step in milliseconds
    pub time_ms: u64,
}

/// Cache status for parse and result caches
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheStatus {
    /// Parse cache hit
    pub parse_cache_hit: bool,
    /// Result cache hit
    pub result_cache_hit: bool,
}

impl QueryPlan {
    /// Current schema version
    pub const SCHEMA_VERSION: u32 = 1;

    /// Create new query plan
    #[must_use]
    pub fn new(
        original_query: String,
        optimized_query: String,
        steps: Vec<ExecutionStep>,
        execution_time_ms: u64,
        used_index: bool,
        cache_status: CacheStatus,
    ) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            original_query,
            optimized_query,
            steps,
            execution_time_ms,
            used_index,
            cache_status,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // QueryPlan tests
    #[test]
    fn test_query_plan_creation() {
        let plan = QueryPlan::new(
            "kind:function".to_string(),
            "kind:function".to_string(),
            vec![],
            10,
            true,
            CacheStatus {
                parse_cache_hit: false,
                result_cache_hit: false,
            },
        );
        assert_eq!(plan.schema_version, QueryPlan::SCHEMA_VERSION);
        assert_eq!(plan.original_query, "kind:function");
        assert_eq!(plan.execution_time_ms, 10);
        assert!(plan.used_index);
    }

    #[test]
    fn test_query_plan_with_steps() {
        let steps = vec![
            ExecutionStep {
                step_num: 1,
                operation: "Parse query".to_string(),
                result_count: 0,
                time_ms: 2,
            },
            ExecutionStep {
                step_num: 2,
                operation: "Index lookup".to_string(),
                result_count: 100,
                time_ms: 5,
            },
            ExecutionStep {
                step_num: 3,
                operation: "Filter results".to_string(),
                result_count: 25,
                time_ms: 3,
            },
        ];

        let plan = QueryPlan::new(
            "kind:function name:test".to_string(),
            "name:test kind:function".to_string(),
            steps.clone(),
            10,
            true,
            CacheStatus {
                parse_cache_hit: false,
                result_cache_hit: false,
            },
        );

        assert_eq!(plan.steps.len(), 3);
        assert_eq!(plan.steps[0].step_num, 1);
        assert_eq!(plan.steps[1].result_count, 100);
        assert_eq!(plan.steps[2].operation, "Filter results");
    }

    #[test]
    fn test_query_plan_optimized_differs() {
        let plan = QueryPlan::new(
            "visibility:public name:foo".to_string(),
            "name:foo visibility:public".to_string(), // Reordered
            vec![],
            15,
            true,
            CacheStatus {
                parse_cache_hit: true,
                result_cache_hit: false,
            },
        );

        assert_ne!(plan.original_query, plan.optimized_query);
        assert!(plan.original_query.starts_with("visibility"));
        assert!(plan.optimized_query.starts_with("name"));
    }

    #[test]
    fn test_query_plan_no_index() {
        let plan = QueryPlan::new(
            "content~=/regex/".to_string(),
            "content~=/regex/".to_string(),
            vec![],
            100,
            false, // Regex search can't use index
            CacheStatus {
                parse_cache_hit: false,
                result_cache_hit: false,
            },
        );

        assert!(!plan.used_index);
    }

    #[test]
    fn test_query_plan_clone() {
        let plan = QueryPlan::new(
            "kind:class".to_string(),
            "kind:class".to_string(),
            vec![ExecutionStep {
                step_num: 1,
                operation: "test".to_string(),
                result_count: 10,
                time_ms: 5,
            }],
            5,
            true,
            CacheStatus {
                parse_cache_hit: true,
                result_cache_hit: true,
            },
        );

        let cloned = plan.clone();
        assert_eq!(plan.original_query, cloned.original_query);
        assert_eq!(plan.steps.len(), cloned.steps.len());
        assert_eq!(
            plan.cache_status.parse_cache_hit,
            cloned.cache_status.parse_cache_hit
        );
    }

    #[test]
    fn test_query_plan_debug() {
        let plan = QueryPlan::new(
            "test".to_string(),
            "test".to_string(),
            vec![],
            0,
            false,
            CacheStatus {
                parse_cache_hit: false,
                result_cache_hit: false,
            },
        );

        let debug_str = format!("{:?}", plan);
        assert!(debug_str.contains("QueryPlan"));
        assert!(debug_str.contains("schema_version"));
    }

    #[test]
    fn test_query_plan_schema_version_constant() {
        assert_eq!(QueryPlan::SCHEMA_VERSION, 1);
    }

    #[test]
    fn test_query_plan_serde_json() {
        let plan = QueryPlan::new(
            "kind:function".to_string(),
            "kind:function".to_string(),
            vec![ExecutionStep {
                step_num: 1,
                operation: "test".to_string(),
                result_count: 5,
                time_ms: 2,
            }],
            10,
            true,
            CacheStatus {
                parse_cache_hit: false,
                result_cache_hit: true,
            },
        );

        let json = serde_json::to_string(&plan).unwrap();
        assert!(json.contains("\"schema_version\":1"));
        assert!(json.contains("\"original_query\":\"kind:function\""));

        let parsed: QueryPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.original_query, plan.original_query);
        assert_eq!(parsed.steps.len(), 1);
    }

    // ExecutionStep tests
    #[test]
    fn test_execution_step() {
        let step = ExecutionStep {
            step_num: 1,
            operation: "Parse query".to_string(),
            result_count: 0,
            time_ms: 1,
        };
        assert_eq!(step.step_num, 1);
        assert_eq!(step.operation, "Parse query");
    }

    #[test]
    fn test_execution_step_clone() {
        let step = ExecutionStep {
            step_num: 2,
            operation: "Index lookup".to_string(),
            result_count: 50,
            time_ms: 10,
        };

        let cloned = step.clone();
        assert_eq!(step.step_num, cloned.step_num);
        assert_eq!(step.operation, cloned.operation);
        assert_eq!(step.result_count, cloned.result_count);
        assert_eq!(step.time_ms, cloned.time_ms);
    }

    #[test]
    fn test_execution_step_debug() {
        let step = ExecutionStep {
            step_num: 1,
            operation: "test".to_string(),
            result_count: 0,
            time_ms: 0,
        };

        let debug_str = format!("{:?}", step);
        assert!(debug_str.contains("ExecutionStep"));
        assert!(debug_str.contains("step_num"));
        assert!(debug_str.contains("operation"));
    }

    #[test]
    fn test_execution_step_serde_json() {
        let step = ExecutionStep {
            step_num: 3,
            operation: "Filter".to_string(),
            result_count: 100,
            time_ms: 15,
        };

        let json = serde_json::to_string(&step).unwrap();
        assert!(json.contains("\"step_num\":3"));
        assert!(json.contains("\"operation\":\"Filter\""));

        let parsed: ExecutionStep = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.step_num, step.step_num);
        assert_eq!(parsed.result_count, step.result_count);
    }

    // CacheStatus tests
    #[test]
    fn test_cache_status() {
        let status = CacheStatus {
            parse_cache_hit: true,
            result_cache_hit: false,
        };
        assert!(status.parse_cache_hit);
        assert!(!status.result_cache_hit);
    }

    #[test]
    fn test_cache_status_both_hit() {
        let status = CacheStatus {
            parse_cache_hit: true,
            result_cache_hit: true,
        };
        assert!(status.parse_cache_hit);
        assert!(status.result_cache_hit);
    }

    #[test]
    fn test_cache_status_both_miss() {
        let status = CacheStatus {
            parse_cache_hit: false,
            result_cache_hit: false,
        };
        assert!(!status.parse_cache_hit);
        assert!(!status.result_cache_hit);
    }

    #[test]
    fn test_cache_status_clone() {
        let status = CacheStatus {
            parse_cache_hit: true,
            result_cache_hit: false,
        };

        let cloned = status.clone();
        assert_eq!(status.parse_cache_hit, cloned.parse_cache_hit);
        assert_eq!(status.result_cache_hit, cloned.result_cache_hit);
    }

    #[test]
    fn test_cache_status_debug() {
        let status = CacheStatus {
            parse_cache_hit: false,
            result_cache_hit: true,
        };

        let debug_str = format!("{:?}", status);
        assert!(debug_str.contains("CacheStatus"));
        assert!(debug_str.contains("parse_cache_hit"));
        assert!(debug_str.contains("result_cache_hit"));
    }

    #[test]
    fn test_cache_status_serde_json() {
        let status = CacheStatus {
            parse_cache_hit: true,
            result_cache_hit: false,
        };

        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("\"parse_cache_hit\":true"));
        assert!(json.contains("\"result_cache_hit\":false"));

        let parsed: CacheStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.parse_cache_hit, status.parse_cache_hit);
        assert_eq!(parsed.result_cache_hit, status.result_cache_hit);
    }
}
