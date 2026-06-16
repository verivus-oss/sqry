//! MCP Prompts for sqry structural semantic code search.
//!
//! This module provides user-facing prompts that appear in Claude Code's
//! `/` menu as `/mcp__sqry__<prompt_name>`. These prompts provide guided
//! workflows for common code search and analysis tasks.
//!
//! **Note on "semantic"**: sqry uses *program semantics* (AST structure, types, call graphs)
//! for deterministic, complete results - not *distributional semantics* (embedding vectors)
//! which provide probabilistic similarity matches.

use rmcp::handler::server::prompt::PromptContext;
use rmcp::handler::server::router::prompt::{PromptRoute, PromptRouter};
use rmcp::model::{GetPromptResult, Prompt, PromptArgument, PromptMessage, PromptMessageRole};

/// Create the prompt router with all sqry prompts.
pub fn create_prompt_router<S: Send + Sync + Clone + 'static>() -> PromptRouter<S> {
    PromptRouter::new()
        .with_route(semantic_search_prompt())
        .with_route(find_callers_prompt())
        .with_route(find_callees_prompt())
        .with_route(trace_path_prompt())
        .with_route(explain_symbol_prompt())
        .with_route(code_impact_prompt())
}

fn prompt_argument(
    name: &str,
    title: &str,
    description: impl Into<String>,
    required: bool,
) -> PromptArgument {
    PromptArgument::new(name)
        .with_title(title)
        .with_description(description)
        .with_required(required)
}

fn prompt_result(description: impl Into<String>, message: String) -> GetPromptResult {
    GetPromptResult::new(vec![PromptMessage::new_text(
        PromptMessageRole::User,
        message,
    )])
    .with_description(description)
}

/// Semantic search prompt - search code by structural meaning (AST-based).
fn semantic_search_prompt<S: Send + Sync + 'static>() -> PromptRoute<S> {
    let prompt = Prompt::new(
        "semantic_search",
        Some(
            "Structural code search - find symbols by name/kind/visibility with 100% precision (not embedding similarity)",
        ),
        Some(vec![
            prompt_argument(
                "query",
                "Search Query",
                "What to search for (e.g., 'authentication functions', 'public classes', 'database handlers')",
                true,
            ),
            prompt_argument(
                "path",
                "Path Filter",
                "Optional directory to limit search (e.g., 'src/auth')",
                false,
            ),
        ]),
    );

    PromptRoute::new_dyn(prompt, |context: PromptContext<'_, S>| {
        Box::pin(async move { Ok(handle_semantic_search(&context)) })
    })
}

fn handle_semantic_search<S>(context: &PromptContext<'_, S>) -> GetPromptResult {
    let query = context
        .arguments
        .as_ref()
        .and_then(|args| args.get("query"))
        .and_then(|v| v.as_str())
        .unwrap_or("functions");

    let path_filter = context
        .arguments
        .as_ref()
        .and_then(|args| args.get("path"))
        .and_then(|v| v.as_str())
        .map(|p| format!(" in path:{p}"))
        .unwrap_or_default();

    let message = format!(
        r#"Use the sqry semantic_search or hierarchical_search tool to find code matching: "{query}"{path_filter}

Note: sqry provides deterministic results via AST analysis (not probabilistic embedding similarity).
Same query → same results. You get the COMPLETE list - critical for refactoring, security audits, and impact analysis.

Translate the user's query into sqry predicates in the `query` parameter:
- For symbol names: use `name:` predicate (e.g., `name:login`, `name~=/.*Handler/`)
- For symbol types: use `kind:` predicate (e.g., `kind:function`, `kind:class`, `kind:method`)
- For visibility: use `visibility:` predicate (e.g., `visibility:public`, `visibility:private`)
- For language: use `lang:` predicate (e.g., `lang:rust`, `lang:typescript`)

Example queries:
- "authentication functions" → semantic_search with query="name~=/^auth/ AND kind:function"
- "public classes" → semantic_search with query="visibility:public AND kind:class"
- "all methods in User class" → semantic_search with query="name~=/^User::/ AND kind:method"

Alternatively, use the `filters` parameter for simple structured constraints:
  filters={{"language":["rust"],"symbol_kind":["function"]}}

Use `query` for complex boolean expressions with AND/OR/NOT/regex.
Use `filters` for simple pre-filtering by language, kind, or visibility.
Both can be combined:
  query="name~=/^auth/" filters={{"language":["typescript"],"visibility":"public"}}

Use hierarchical_search for RAG-optimized results with file/container grouping."#
    );

    prompt_result(format!("Search for code matching: {query}"), message)
}

/// Find callers prompt - discover what calls a function.
fn find_callers_prompt<S: Send + Sync + 'static>() -> PromptRoute<S> {
    let prompt = Prompt::new(
        "find_callers",
        Some("Find all code that calls a specific function or method"),
        Some(vec![prompt_argument(
            "symbol",
            "Symbol Name",
            "The function or method to find callers for (e.g., 'authenticate', 'User::save')",
            true,
        )]),
    );

    PromptRoute::new_dyn(prompt, |context: PromptContext<'_, S>| {
        Box::pin(async move { Ok(handle_find_callers(&context)) })
    })
}

fn handle_find_callers<S>(context: &PromptContext<'_, S>) -> GetPromptResult {
    let symbol = context
        .arguments
        .as_ref()
        .and_then(|args| args.get("symbol"))
        .and_then(|v| v.as_str())
        .unwrap_or("main");

    let message = format!(
        r#"Use the sqry relation_query tool to find all callers of "{symbol}".

Call relation_query with:
- symbol: "{symbol}"
- relation_type: "callers"
- max_depth: 2 (increase for transitive callers)

This will show all functions/methods that call {symbol}, helping understand:
- Who depends on this code
- Impact of changing this function
- Call patterns in the codebase"#
    );

    prompt_result(format!("Find all code that calls: {symbol}"), message)
}

/// Find callees prompt - discover what a function calls.
fn find_callees_prompt<S: Send + Sync + 'static>() -> PromptRoute<S> {
    let prompt = Prompt::new(
        "find_callees",
        Some("Find all functions/methods that a specific function calls"),
        Some(vec![prompt_argument(
            "symbol",
            "Symbol Name",
            "The function to analyze (e.g., 'process_request', 'main')",
            true,
        )]),
    );

    PromptRoute::new_dyn(prompt, |context: PromptContext<'_, S>| {
        Box::pin(async move { Ok(handle_find_callees(&context)) })
    })
}

fn handle_find_callees<S>(context: &PromptContext<'_, S>) -> GetPromptResult {
    let symbol = context
        .arguments
        .as_ref()
        .and_then(|args| args.get("symbol"))
        .and_then(|v| v.as_str())
        .unwrap_or("main");

    let message = format!(
        r#"Use the sqry relation_query tool to find all functions called by "{symbol}".

Call relation_query with:
- symbol: "{symbol}"
- relation_type: "callees"
- max_depth: 2 (increase for transitive callees)

This will show all functions/methods that {symbol} calls, helping understand:
- Dependencies of this function
- What subsystems it touches
- Complexity and coupling"#
    );

    prompt_result(format!("Find all functions called by: {symbol}"), message)
}

/// Trace path prompt - find call paths between two symbols.
fn trace_path_prompt<S: Send + Sync + 'static>() -> PromptRoute<S> {
    let prompt = Prompt::new(
        "trace_path",
        Some("Trace the call path between two functions - how does A eventually call B?"),
        Some(vec![
            prompt_argument(
                "from",
                "Starting Function",
                "The function where the path starts (e.g., 'main', 'handle_request')",
                true,
            ),
            prompt_argument(
                "to",
                "Target Function",
                "The function where the path ends (e.g., 'database_query', 'send_email')",
                true,
            ),
        ]),
    );

    PromptRoute::new_dyn(prompt, |context: PromptContext<'_, S>| {
        Box::pin(async move { Ok(handle_trace_path(&context)) })
    })
}

fn handle_trace_path<S>(context: &PromptContext<'_, S>) -> GetPromptResult {
    let from = context
        .arguments
        .as_ref()
        .and_then(|args| args.get("from"))
        .and_then(|v| v.as_str())
        .unwrap_or("main");

    let to = context
        .arguments
        .as_ref()
        .and_then(|args| args.get("to"))
        .and_then(|v| v.as_str())
        .unwrap_or("target");

    let message = format!(
        r#"Use the sqry trace_path tool to find how "{from}" reaches "{to}".

Call trace_path with:
- from_symbol: "{from}"
- to_symbol: "{to}"
- max_hops: 5 (increase if path might be longer)
- max_paths: 3 (to see alternative routes)

This will show the call chain from {from} to {to}, helping understand:
- How control flows through the codebase
- Critical paths for debugging
- Dependencies between subsystems"#
    );

    prompt_result(format!("Trace call path from {from} to {to}"), message)
}

/// Explain symbol prompt - get detailed information about a symbol.
fn explain_symbol_prompt<S: Send + Sync + 'static>() -> PromptRoute<S> {
    let prompt = Prompt::new(
        "explain_symbol",
        Some("Get detailed explanation of a code symbol including its context and relationships"),
        Some(vec![
            prompt_argument(
                "file",
                "File Path",
                "Path to the file containing the symbol (e.g., 'src/auth/login.rs')",
                true,
            ),
            prompt_argument(
                "symbol",
                "Symbol Name",
                "Name of the symbol to explain (e.g., 'authenticate', 'UserService')",
                true,
            ),
        ]),
    );

    PromptRoute::new_dyn(prompt, |context: PromptContext<'_, S>| {
        Box::pin(async move { Ok(handle_explain_symbol(&context)) })
    })
}

fn handle_explain_symbol<S>(context: &PromptContext<'_, S>) -> GetPromptResult {
    let file = context
        .arguments
        .as_ref()
        .and_then(|args| args.get("file"))
        .and_then(|v| v.as_str())
        .unwrap_or("src/main.rs");

    let symbol = context
        .arguments
        .as_ref()
        .and_then(|args| args.get("symbol"))
        .and_then(|v| v.as_str())
        .unwrap_or("main");

    let message = format!(
        r#"Use the sqry explain_code tool to get detailed information about "{symbol}" in {file}.

Call explain_code with:
- file_path: "{file}"
- symbol_name: "{symbol}"
- include_context: true
- include_relations: true

This will provide:
- Symbol signature and documentation
- Surrounding context code
- Callers and callees relationships
- Import/export information"#
    );

    prompt_result(format!("Explain symbol {symbol} in {file}"), message)
}

/// Code impact prompt - analyze what would break if a symbol changes.
fn code_impact_prompt<S: Send + Sync + 'static>() -> PromptRoute<S> {
    let prompt = Prompt::new(
        "code_impact",
        Some("Analyze what code would be affected if a symbol is changed or removed"),
        Some(vec![prompt_argument(
            "symbol",
            "Symbol Name",
            "The symbol to analyze impact for (e.g., 'UserService', 'validate_input')",
            true,
        )]),
    );

    PromptRoute::new_dyn(prompt, |context: PromptContext<'_, S>| {
        Box::pin(async move { Ok(handle_code_impact(&context)) })
    })
}

fn handle_code_impact<S>(context: &PromptContext<'_, S>) -> GetPromptResult {
    let symbol = context
        .arguments
        .as_ref()
        .and_then(|args| args.get("symbol"))
        .and_then(|v| v.as_str())
        .unwrap_or("target");

    let message = format!(
        r#"Use the sqry dependency_impact tool to analyze what would be affected by changing "{symbol}".

Call dependency_impact with:
- symbol: "{symbol}"
- max_depth: 3
- include_indirect: true
- include_files: true

This will show:
- Direct dependents (code that directly uses this symbol)
- Indirect dependents (transitive impact)
- Affected files list
- Risk assessment for the change"#
    );

    prompt_result(format!("Analyze impact of changing: {symbol}"), message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prompt_router_creation() {
        let router: PromptRouter<()> = create_prompt_router();
        let prompts = router.list_all();

        assert!(prompts.len() >= 6);

        let names: Vec<&str> = prompts.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"semantic_search"));
        assert!(names.contains(&"find_callers"));
        assert!(names.contains(&"find_callees"));
        assert!(names.contains(&"trace_path"));
        assert!(names.contains(&"explain_symbol"));
        assert!(names.contains(&"code_impact"));
        assert!(!names.contains(&"ask"));
    }

    #[test]
    fn test_semantic_search_prompt_has_arguments() {
        let router: PromptRouter<()> = create_prompt_router();
        let prompts = router.list_all();

        let search_prompt = prompts
            .iter()
            .find(|p| p.name == "semantic_search")
            .unwrap();

        assert!(search_prompt.arguments.is_some());
        let args = search_prompt.arguments.as_ref().unwrap();
        assert!(args.iter().any(|a| a.name == "query"));
    }
}
