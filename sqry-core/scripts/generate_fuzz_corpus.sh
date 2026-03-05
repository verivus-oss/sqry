#!/usr/bin/env bash
# Generate fuzz corpus from parser unit tests
# Part of P2-37 Phase 1 Fix #4: Query Parser Fuzz Testing

set -euo pipefail

CORPUS_DIR="fuzz/corpus/query_parser/seeds"
mkdir -p "$CORPUS_DIR"

echo "Generating fuzz corpus seeds from parser unit tests..."

# Valid queries (from unit tests)
echo "kind:function" > "$CORPUS_DIR/001_simple_condition"
echo "kind:function AND async:true" > "$CORPUS_DIR/002_and_expression"
echo "kind:function OR kind:class" > "$CORPUS_DIR/003_or_expression"
echo "NOT kind:function" > "$CORPUS_DIR/004_not_expression"
echo "a:1 OR b:2 AND c:3" > "$CORPUS_DIR/005_precedence_or_and"
echo "(kind:function)" > "$CORPUS_DIR/006_parentheses"
echo "((a:1))" > "$CORPUS_DIR/007_nested_parentheses"
echo "(kind:function OR kind:method) AND async:true AND NOT name~=/test/" > "$CORPUS_DIR/008_complex_query"
echo "name~=/^test_/i" > "$CORPUS_DIR/009_regex_pattern"
echo "scope.type:class" > "$CORPUS_DIR/010_scope_fields"

# Invalid queries (error cases from tests)
echo "" > "$CORPUS_DIR/011_empty_query"
echo "(kind:function" > "$CORPUS_DIR/012_unmatched_paren"
echo "kind:" > "$CORPUS_DIR/013_missing_value"
echo "kind:function AND" > "$CORPUS_DIR/014_incomplete_and"
echo "kind:function OR" > "$CORPUS_DIR/015_incomplete_or"
echo "NOT" > "$CORPUS_DIR/016_incomplete_not"
echo "kind function" > "$CORPUS_DIR/017_missing_operator"

# Edge cases
echo "a:1 OR b:2 AND c:3 OR d:4" > "$CORPUS_DIR/018_precedence_complex"
echo "(a:1 OR b:2) AND c:3" > "$CORPUS_DIR/019_parens_override_precedence"
echo "NOT NOT kind:function" > "$CORPUS_DIR/020_double_not"
echo "NOT NOT NOT kind:function" > "$CORPUS_DIR/021_triple_not"
echo "a:1 AND b:2 AND c:3 AND d:4" > "$CORPUS_DIR/022_chained_and"
echo "a:1 OR b:2 OR c:3 OR d:4" > "$CORPUS_DIR/023_chained_or"

# Comparison operators
echo "a:b" > "$CORPUS_DIR/024_equal"
echo "a~=/b/" > "$CORPUS_DIR/025_regex"
echo "a>1" > "$CORPUS_DIR/026_greater"
echo "a<1" > "$CORPUS_DIR/027_less"
echo "a>=1" > "$CORPUS_DIR/028_greater_eq"
echo "a<=1" > "$CORPUS_DIR/029_less_eq"

# Value types
echo 'name:"hello world"' > "$CORPUS_DIR/030_string_literal"
echo "lines:42" > "$CORPUS_DIR/031_number"
echo "async:true" > "$CORPUS_DIR/032_boolean_true"
echo "async:false" > "$CORPUS_DIR/033_boolean_false"
echo "name~=/^test_/i" > "$CORPUS_DIR/034_regex_flags"

# Plain words (legacy syntax)
echo "Error" > "$CORPUS_DIR/035_plain_word"
echo '"Error"' > "$CORPUS_DIR/036_plain_string_literal"

# Complex scope queries
echo "scope.type:class AND name:connect" > "$CORPUS_DIR/037_scope_and_kind"
echo "name:connect AND scope.ancestor:UserModule" > "$CORPUS_DIR/038_scope_with_and"
echo "NOT scope.type:test" > "$CORPUS_DIR/039_not_scope_type"
echo "scope.type:class AND (name:connect OR name:disconnect)" > "$CORPUS_DIR/040_complex_scope"
echo "scope.type:function AND scope.parent:Database AND scope.ancestor:UserModule" > "$CORPUS_DIR/041_multiple_scope_filters"

# Deeply nested expressions
echo "((A AND B) OR (C AND D)) AND E" > "$CORPUS_DIR/042_deeply_nested"
echo "NOT (a:1 OR b:2)" > "$CORPUS_DIR/043_not_with_parentheses"
echo "(A OR B) AND (C OR D)" > "$CORPUS_DIR/044_multiple_groups"

# Edge cases - whitespace and length (Codex review requirement)
echo "   " > "$CORPUS_DIR/045_whitespace_only"
echo "((((((((kind:function AND async:true) OR (name:test AND scope.type:class)) AND (path:src OR path:lib)) OR (lang:rust AND NOT name~=/^_/)) AND (lines>100 AND lines<1000)) OR (kind:method AND scope.parent:Database)) AND (text~=/TODO/ OR text~=/FIXME/)) OR (kind:class AND name~=/^Test/))" > "$CORPUS_DIR/046_deeply_nested_long_query"

echo "Generated $(ls -1 $CORPUS_DIR | wc -l) corpus files"
