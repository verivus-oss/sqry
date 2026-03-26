//! Command template assembly.
//!
//! Maps (intent, entities) to validated sqry command strings.

mod formatting;
pub(crate) mod templates;

use crate::error::{AssemblerError, NlResult};
use crate::types::{ExtractedEntities, Intent, PredicateType, TemplateType, Visibility};

/// Convenience function to assemble a command from intent and entities.
///
/// Uses default assembler configuration.
///
/// # Errors
///
/// Returns [`AssemblerError`] if assembly fails.
pub fn assemble_command(
    intent: &Intent,
    entities: &ExtractedEntities,
) -> Result<String, AssemblerError> {
    let assembler = TemplateAssembler::default();
    assembler
        .assemble(*intent, entities)
        .map(|cmd| cmd.command)
        .map_err(|e| match e {
            crate::error::NlError::Assembler(ae) => ae,
            _ => AssemblerError::AmbiguousIntent, // Fallback for unexpected errors
        })
}

/// Assembled command result.
#[derive(Debug, Clone)]
pub struct AssembledCommand {
    /// The assembled command string.
    pub command: String,
    /// The template type used.
    pub template_type: TemplateType,
}

/// Command assembler configuration.
#[derive(Debug, Clone)]
pub struct AssemblerConfig {
    /// Default limit when not specified in query
    pub default_limit: u32,
    /// Maximum command length
    pub max_command_length: usize,
    /// Default max depth for graph queries
    pub default_max_depth: u32,
}

impl Default for AssemblerConfig {
    fn default() -> Self {
        Self {
            default_limit: 100,
            max_command_length: 512,
            default_max_depth: 10,
        }
    }
}

/// Template assembler for building sqry commands.
pub struct TemplateAssembler {
    config: AssemblerConfig,
}

impl Default for TemplateAssembler {
    fn default() -> Self {
        Self::new(AssemblerConfig::default())
    }
}

impl TemplateAssembler {
    /// Create a new assembler with the given configuration.
    #[must_use]
    pub fn new(config: AssemblerConfig) -> Self {
        Self { config }
    }

    /// Assemble a command from intent and entities.
    ///
    /// # Errors
    ///
    /// Returns [`AssemblerError`] if:
    /// - Intent is Ambiguous
    /// - Required entities are missing
    /// - Generated command exceeds length limit
    pub fn assemble(
        &self,
        intent: Intent,
        entities: &ExtractedEntities,
    ) -> NlResult<AssembledCommand> {
        match intent {
            Intent::SymbolQuery => self.build_query_command(entities),
            Intent::TextSearch => self.build_search_command(entities),
            Intent::TracePath => self.build_trace_path_command(entities),
            Intent::FindCallers => self.build_callers_command(entities),
            Intent::FindCallees => self.build_callees_command(entities),
            Intent::Visualize => self.build_visualize_command(entities),
            Intent::IndexStatus => self.build_index_status_command(entities),
            Intent::Ambiguous => Err(AssemblerError::AmbiguousIntent.into()),
        }
    }

    fn build_command(
        &self,
        parts: &[String],
        template_type: TemplateType,
    ) -> NlResult<AssembledCommand> {
        let command = parts.join(" ");
        self.validate_length(&command)?;

        Ok(AssembledCommand {
            command,
            template_type,
        })
    }

    fn push_languages(parts: &mut Vec<String>, languages: &[String]) {
        for lang in languages {
            parts.push(format!("--language {lang}"));
        }
    }

    fn push_path(parts: &mut Vec<String>, paths: &[String]) {
        if let Some(path) = paths.first() {
            parts.push(format!("--path \"{}\"", formatting::escape_quotes(path)));
        }
    }

    fn require_primary_symbol(
        entities: &ExtractedEntities,
        error: AssemblerError,
    ) -> NlResult<&str> {
        entities.primary_symbol().ok_or_else(|| error.into())
    }

    fn build_query_command(&self, entities: &ExtractedEntities) -> NlResult<AssembledCommand> {
        // Check if this is a CD predicate query first
        let query_expr = Self::build_query_expression(entities)?;

        let mut parts = vec![
            "sqry".to_string(),
            "query".to_string(),
            format!("\"{}\"", formatting::escape_quotes(&query_expr)),
        ];

        // Add language filters
        Self::push_languages(&mut parts, &entities.languages);

        // Add path filter
        Self::push_path(&mut parts, &entities.paths);

        // Note: kind filter is now included in query expression as `kind:X` predicate
        // (see build_query_expression)

        // Add limit
        let limit = entities.limit.unwrap_or(self.config.default_limit);
        parts.push(format!("--limit {limit}"));

        self.build_command(&parts, TemplateType::Query)
    }

    /// Build the query expression, handling CD predicates.
    ///
    /// Returns the query expression string (e.g., "impl:Future", "duplicates:body", etc.)
    fn build_query_expression(entities: &ExtractedEntities) -> NlResult<String> {
        let mut expr_parts = Self::collect_predicates(entities);

        if expr_parts.is_empty() {
            return Self::build_symbol_only_query(entities);
        }

        if let Some(symbol) = entities.primary_symbol()
            && Self::should_include_symbol(entities, symbol)
        {
            expr_parts.push(symbol.to_string());
        }

        // Use AND to join predicates - required for correct parsing
        // when boolean predicates like async:true are combined with other predicates
        Ok(expr_parts.join(" AND "))
    }

    fn build_search_command(&self, entities: &ExtractedEntities) -> NlResult<AssembledCommand> {
        let pattern = Self::require_primary_symbol(entities, AssemblerError::MissingSymbol)?;

        let mut parts = vec![
            "sqry".to_string(),
            "search".to_string(),
            format!("\"{}\"", formatting::escape_quotes(pattern)),
        ];

        // Add language filters
        Self::push_languages(&mut parts, &entities.languages);

        // Add path filter
        Self::push_path(&mut parts, &entities.paths);

        self.build_command(&parts, TemplateType::Search)
    }

    fn build_trace_path_command(&self, entities: &ExtractedEntities) -> NlResult<AssembledCommand> {
        let from = entities
            .from_symbol
            .as_deref()
            .or_else(|| entities.symbols.first().map(String::as_str))
            .ok_or(AssemblerError::MissingTracePath)?;

        let to = entities
            .to_symbol
            .as_deref()
            .or_else(|| entities.symbols.get(1).map(String::as_str))
            .ok_or(AssemblerError::MissingTracePath)?;

        let mut parts = vec![
            "sqry".to_string(),
            "graph".to_string(),
            "trace-path".to_string(),
            format!("\"{}\"", formatting::escape_quotes(from)),
            format!("\"{}\"", formatting::escape_quotes(to)),
        ];

        // Add max-depth
        let depth = entities.depth.unwrap_or(self.config.default_max_depth);
        parts.push(format!("--max-depth {depth}"));

        let command = parts.join(" ");
        self.validate_length(&command)?;

        Ok(AssembledCommand {
            command,
            template_type: TemplateType::TracePath,
        })
    }

    fn build_callers_command(&self, entities: &ExtractedEntities) -> NlResult<AssembledCommand> {
        let symbol = Self::require_primary_symbol(entities, AssemblerError::MissingSymbol)?;

        let mut parts = vec![
            "sqry".to_string(),
            "graph".to_string(),
            "direct-callers".to_string(),
            format!("\"{}\"", formatting::escape_quotes(symbol)),
        ];

        // Single language only for graph commands
        if let Some(lang) = entities.languages.first() {
            parts.push(format!("--language {lang}"));
        }

        self.build_command(&parts, TemplateType::GraphCallers)
    }

    fn build_callees_command(&self, entities: &ExtractedEntities) -> NlResult<AssembledCommand> {
        let symbol = Self::require_primary_symbol(entities, AssemblerError::MissingSymbol)?;

        let mut parts = vec![
            "sqry".to_string(),
            "graph".to_string(),
            "direct-callees".to_string(),
            format!("\"{}\"", formatting::escape_quotes(symbol)),
        ];

        // Single language only for graph commands
        if let Some(lang) = entities.languages.first() {
            parts.push(format!("--language {lang}"));
        }

        self.build_command(&parts, TemplateType::GraphCallees)
    }

    fn build_visualize_command(&self, entities: &ExtractedEntities) -> NlResult<AssembledCommand> {
        let symbol = Self::require_primary_symbol(entities, AssemblerError::MissingSymbol)?;

        let relation = entities.relation.as_deref().unwrap_or("call");

        let mut parts = vec![
            "sqry".to_string(),
            "visualize".to_string(),
            format!("--relation {}", relation),
            format!("--symbol \"{}\"", formatting::escape_quotes(symbol)),
        ];

        // Add format
        if let Some(format) = entities.format {
            parts.push(format!("--format {}", format.as_str()));
        }

        self.build_command(&parts, TemplateType::Visualize)
    }

    fn build_index_status_command(
        &self,
        entities: &ExtractedEntities,
    ) -> NlResult<AssembledCommand> {
        let mut parts = vec![
            "sqry".to_string(),
            "index".to_string(),
            "--status".to_string(),
        ];

        // Add path filter
        if let Some(path) = entities.paths.first() {
            parts.push(format!("--path \"{}\"", formatting::escape_quotes(path)));
        }

        // Add JSON flag if requested
        if entities.format == Some(crate::types::OutputFormat::Json) {
            parts.push("--json".to_string());
        }

        self.build_command(&parts, TemplateType::IndexStatus)
    }

    fn collect_predicates(entities: &ExtractedEntities) -> Vec<String> {
        let mut expr_parts = Vec::new();

        // 1. Handle impl: predicate
        if let Some(trait_name) = &entities.impl_trait {
            expr_parts.push(format!("impl:{trait_name}"));
        }

        // 2. Handle duplicates: predicate (default to "body" if no arg specified)
        if entities.predicate_type == Some(PredicateType::Duplicates) {
            let arg = entities.predicate_arg.as_deref().unwrap_or("body");
            expr_parts.push(format!("duplicates:{arg}"));
        }

        // 3. Handle circular: predicate (default to "calls" if no arg specified)
        if entities.predicate_type == Some(PredicateType::Circular) {
            let arg = entities.predicate_arg.as_deref().unwrap_or("calls");
            expr_parts.push(format!("circular:{arg}"));
        }

        // 4. Handle unused: predicate
        if entities.predicate_type == Some(PredicateType::Unused) {
            expr_parts.push("unused:".to_string());
        }

        // 5. Handle visibility filter
        if let Some(visibility) = entities.visibility {
            match visibility {
                Visibility::Public => expr_parts.push("visibility:public".to_string()),
                Visibility::Private => expr_parts.push("visibility:private".to_string()),
            }
        }

        // 6. Handle async filter
        if entities.is_async == Some(true) {
            expr_parts.push("async:true".to_string());
        }

        // 7. Handle unsafe filter
        if entities.is_unsafe == Some(true) {
            expr_parts.push("unsafe:true".to_string());
        }

        // 8. Handle kind filter as query predicate
        if let Some(kind) = entities.kind {
            expr_parts.push(format!("kind:{}", kind.as_str()));
        }

        expr_parts
    }

    fn build_symbol_only_query(entities: &ExtractedEntities) -> NlResult<String> {
        match entities.primary_symbol() {
            Some(symbol) => Ok(symbol.to_string()),
            None if entities.kind.is_some() => Ok("*".to_string()),
            None => Err(AssemblerError::MissingSymbol.into()),
        }
    }

    fn should_include_symbol(entities: &ExtractedEntities, symbol: &str) -> bool {
        entities.impl_trait.is_none() || symbol != entities.impl_trait.as_deref().unwrap_or("")
    }

    fn validate_length(&self, command: &str) -> NlResult<()> {
        if command.len() > self.config.max_command_length {
            return Err(AssemblerError::CommandTooLong {
                len: command.len(),
                max: self.config.max_command_length,
            }
            .into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{PredicateType, SymbolKind, Visibility};

    #[test]
    fn test_build_query_basic() {
        let assembler = TemplateAssembler::default();
        let mut entities = ExtractedEntities::new();
        entities.symbols.push("authenticate".to_string());

        let result = assembler.assemble(Intent::SymbolQuery, &entities).unwrap();
        assert!(result.command.starts_with("sqry query"));
        assert!(result.command.contains("\"authenticate\""));
    }

    #[test]
    fn test_build_query_with_options() {
        let assembler = TemplateAssembler::default();
        let mut entities = ExtractedEntities::new();
        entities.symbols.push("foo".to_string());
        entities.languages.push("rust".to_string());
        entities.kind = Some(SymbolKind::Function);
        entities.limit = Some(10);

        let result = assembler.assemble(Intent::SymbolQuery, &entities).unwrap();
        assert!(result.command.contains("--language rust"));
        // kind is now part of the query expression, not a CLI flag
        assert!(result.command.contains("kind:function"));
        assert!(result.command.contains("--limit 10"));
    }

    #[test]
    fn test_build_callers() {
        let assembler = TemplateAssembler::default();
        let mut entities = ExtractedEntities::new();
        entities.symbols.push("login".to_string());

        let result = assembler.assemble(Intent::FindCallers, &entities).unwrap();
        assert!(result.command.contains("sqry graph direct-callers"));
        assert!(result.command.contains("\"login\""));
    }

    #[test]
    fn test_build_trace_path() {
        let assembler = TemplateAssembler::default();
        let mut entities = ExtractedEntities::new();
        entities.from_symbol = Some("login".to_string());
        entities.to_symbol = Some("database".to_string());

        let result = assembler.assemble(Intent::TracePath, &entities).unwrap();
        assert!(result.command.contains("sqry graph trace-path"));
        assert!(result.command.contains("\"login\""));
        assert!(result.command.contains("\"database\""));
    }

    #[test]
    fn test_missing_symbol_error() {
        let assembler = TemplateAssembler::default();
        let entities = ExtractedEntities::new();

        let result = assembler.assemble(Intent::SymbolQuery, &entities);
        assert!(matches!(
            result,
            Err(crate::error::NlError::Assembler(
                AssemblerError::MissingSymbol
            ))
        ));
    }

    #[test]
    fn test_ambiguous_intent_error() {
        let assembler = TemplateAssembler::default();
        let entities = ExtractedEntities::new();

        let result = assembler.assemble(Intent::Ambiguous, &entities);
        assert!(matches!(
            result,
            Err(crate::error::NlError::Assembler(
                AssemblerError::AmbiguousIntent
            ))
        ));
    }

    // --- CD Predicate tests ---

    #[test]
    fn test_build_query_impl_predicate() {
        let assembler = TemplateAssembler::default();
        let mut entities = ExtractedEntities::new();
        entities.impl_trait = Some("Future".to_string());
        entities.predicate_type = Some(PredicateType::Impl);

        let result = assembler.assemble(Intent::SymbolQuery, &entities).unwrap();
        assert!(result.command.contains("\"impl:Future\""));
        assert!(result.command.starts_with("sqry query"));
    }

    #[test]
    fn test_build_query_duplicates_predicate() {
        let assembler = TemplateAssembler::default();
        let mut entities = ExtractedEntities::new();
        entities.predicate_type = Some(PredicateType::Duplicates);

        let result = assembler.assemble(Intent::SymbolQuery, &entities).unwrap();
        // Default to "body" when no arg specified
        assert!(result.command.contains("\"duplicates:body\""));
    }

    #[test]
    fn test_build_query_duplicates_signature() {
        let assembler = TemplateAssembler::default();
        let mut entities = ExtractedEntities::new();
        entities.predicate_type = Some(PredicateType::Duplicates);
        entities.predicate_arg = Some("signature".to_string());

        let result = assembler.assemble(Intent::SymbolQuery, &entities).unwrap();
        assert!(result.command.contains("\"duplicates:signature\""));
    }

    #[test]
    fn test_build_query_circular_predicate() {
        let assembler = TemplateAssembler::default();
        let mut entities = ExtractedEntities::new();
        entities.predicate_type = Some(PredicateType::Circular);

        let result = assembler.assemble(Intent::SymbolQuery, &entities).unwrap();
        // Default to "calls" when no arg specified
        assert!(result.command.contains("\"circular:calls\""));
    }

    #[test]
    fn test_build_query_unused_predicate() {
        let assembler = TemplateAssembler::default();
        let mut entities = ExtractedEntities::new();
        entities.predicate_type = Some(PredicateType::Unused);

        let result = assembler.assemble(Intent::SymbolQuery, &entities).unwrap();
        assert!(result.command.contains("\"unused:\""));
    }

    #[test]
    fn test_build_query_visibility_public() {
        let assembler = TemplateAssembler::default();
        let mut entities = ExtractedEntities::new();
        entities.visibility = Some(Visibility::Public);

        let result = assembler.assemble(Intent::SymbolQuery, &entities).unwrap();
        assert!(result.command.contains("visibility:public"));
    }

    #[test]
    fn test_build_query_async_predicate() {
        let assembler = TemplateAssembler::default();
        let mut entities = ExtractedEntities::new();
        entities.is_async = Some(true);

        let result = assembler.assemble(Intent::SymbolQuery, &entities).unwrap();
        assert!(result.command.contains("async:true"));
    }

    #[test]
    fn test_build_query_unsafe_predicate() {
        let assembler = TemplateAssembler::default();
        let mut entities = ExtractedEntities::new();
        entities.is_unsafe = Some(true);

        let result = assembler.assemble(Intent::SymbolQuery, &entities).unwrap();
        assert!(result.command.contains("unsafe:true"));
    }

    #[test]
    fn test_build_query_combined_predicates() {
        let assembler = TemplateAssembler::default();
        let mut entities = ExtractedEntities::new();
        entities.visibility = Some(Visibility::Public);
        entities.is_async = Some(true);

        let result = assembler.assemble(Intent::SymbolQuery, &entities).unwrap();
        assert!(result.command.contains("visibility:public"));
        assert!(result.command.contains("async:true"));
    }

    #[test]
    fn test_build_query_impl_with_symbol_no_duplicate() {
        let assembler = TemplateAssembler::default();
        let mut entities = ExtractedEntities::new();
        entities.impl_trait = Some("Iterator".to_string());
        entities.predicate_type = Some(PredicateType::Impl);
        // Symbol that matches trait name should not be duplicated
        entities.symbols.push("Iterator".to_string());

        let result = assembler.assemble(Intent::SymbolQuery, &entities).unwrap();
        // Should only have impl:Iterator once, not "impl:Iterator Iterator"
        assert!(result.command.contains("\"impl:Iterator\""));
        // Count occurrences of "Iterator" in the command
        let count = result.command.matches("Iterator").count();
        assert_eq!(
            count, 1,
            "Iterator should only appear once in: {}",
            result.command
        );
    }
}

// Predicate assembly regression tests
#[cfg(test)]
mod predicate_assembly_tests {
    use super::*;
    use crate::extractor::extract_entities;

    #[test]
    fn test_async_functions_assembly() {
        let assembler = TemplateAssembler::default();
        let entities = extract_entities("find async functions");
        let result = assembler.assemble(Intent::SymbolQuery, &entities);

        assert!(result.is_ok());
        let cmd = result.unwrap();
        assert!(cmd.command.contains("async:true"));
    }

    #[test]
    fn test_unsafe_functions_assembly() {
        let assembler = TemplateAssembler::default();
        let entities = extract_entities("find unsafe functions");
        let result = assembler.assemble(Intent::SymbolQuery, &entities);

        assert!(result.is_ok());
        let cmd = result.unwrap();
        assert!(cmd.command.contains("unsafe:true"));
    }

    #[test]
    fn test_public_async_functions_assembly() {
        let assembler = TemplateAssembler::default();
        let entities = extract_entities("find public async functions");
        let result = assembler.assemble(Intent::SymbolQuery, &entities);

        assert!(result.is_ok());
        let cmd = result.unwrap();
        assert!(cmd.command.contains("visibility:public"));
        assert!(cmd.command.contains("async:true"));
    }
}
