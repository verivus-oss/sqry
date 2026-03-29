#!/usr/bin/env python3
"""
Training data generation for sqry-nl intent classifier.

Key principles (from H6 risk mitigation):
1. Use structured templates, not LLM-generated data
2. Include negative examples (ambiguous queries)
3. Manual verification of 10% sample required

Usage:
    python generate_data.py --output data/train.json --samples 1000
    python generate_data.py --output data/test.json --samples 200 --seed 42
"""

import json
import hashlib
import secrets
from pathlib import Path
from typing import Callable, MutableSequence, Sequence, TypedDict, TypeVar
from dataclasses import dataclass, field
import typer
from rich.console import Console
from rich.progress import Progress, SpinnerColumn, TextColumn

console = Console()
app = typer.Typer()

T = TypeVar("T")

# Intent definitions matching sqry-nl/src/types.rs
INTENTS = [
    "SymbolQuery",
    "TextSearch",
    "TracePath",
    "FindCallers",
    "FindCallees",
    "Visualize",
    "IndexStatus",
    "Ambiguous",
]
MIN_SAMPLES_PER_INTENT = 1000

# Common template patterns (avoid duplication per S1192)
_FIND_CIRCULAR_DEPS = "find circular dependencies"

# Template-based training data (H6 mitigation: no LLM generation)
# Expanded to cover edge cases from golden_queries.toml for improved accuracy
# Enhanced with sqry-specific predicates and parameters
INTENT_TEMPLATES: dict[str, list[str]] = {
    "SymbolQuery": [
        # Basic find patterns
        "find {symbol}",
        "find {symbol} {kind}",
        "find the {symbol} {kind}",
        "where is {symbol} defined",
        "where is {symbol}",
        "show me {symbol}",
        "show {symbol}",
        "locate {symbol}",
        "search for {symbol}",
        # Kind-specific patterns
        "find {kind} {symbol}",
        "find {kind} named {symbol}",
        "find {kind} called {symbol}",
        "find {symbol} {kind} in {language}",
        "find all {kind}s named {symbol}",
        "find all {kind}s",
        "list all {kind}s",
        "show all {kind}s",
        "{kind}s named {symbol}",
        # Language-specific patterns
        "find {symbol} in {language}",
        "search {symbol} in {language}",
        "find {symbol} in {language} files",
        "{symbol} in {language}",
        "{symbol} {kind} in {language}",
        "show {kind}s in {language}",
        "{kind}s in {language}",
        # Location patterns
        "where is {symbol} implemented",
        "where is {symbol} class",
        "where is the {kind} {symbol}",
        "definition of {symbol}",
        "go to {symbol}",
        "look up {symbol}",
        # Visibility patterns
        "find public {kind}s",
        "find private {kind}s",
        "show me all public {kind}s",
        "private {kind}s in {symbol}",
        # Path-specific patterns
        "find all {kind}s in {path}",
        "show {kind}s in {path}",
        "{kind}s in {path}",
        "find {symbol} in {path}",
        "show constants in {path}",
        # Limit patterns
        "find {symbol} with limit {limit}",
        "first {limit} {kind}s",
        "top {limit} {kind}s named {symbol}",
        "show first {limit} {symbol}",
        # Show/list variations
        "show {symbol} definition",
        "locate the {kind} {symbol}",
        "show me all {kind}s",
        "list all {kind}s in {language}",
        "list {kind}s",
        # Error handling patterns
        "find all error handling {kind}s",
        "find error {symbol}",
        # Module patterns
        "show me modules in {path}",
        "find modules",
        "list modules",
        # Type patterns
        "find {symbol} type alias",
        "find type {symbol}",
        "{symbol} type",
        # Authentication patterns
        "authentication {kind}s in {language}",
        "find authentication {kind}",
        # HARD NEGATIVES: Patterns that look like TextSearch but are SymbolQuery
        # (looking for actual code symbols, not text patterns)
        "find the Error {kind}",
        "find the Result {kind}",
        "where is Error defined",
        "where is the Config {kind}",
        "find the {kind} Error",
        "find Logger {kind}",
        "find Handler {kind} in {language}",
        "locate the Error handler",
        "show me the Error type",
        "find {kind} Config",
        "find {kind} Logger",
        "find the validate {kind}",
        "find {kind} validate_input",
        "find the authenticate {kind}",
        "where is parse defined",
        "find the parse {kind}",
        "show {kind} named handle",
        "find process {kind}",
        "locate config {kind}",
        "find callback {kind}",
        # ============================================
        # SQRY-SPECIFIC PREDICATE PATTERNS (query DSL)
        # ============================================
        # kind: predicate patterns
        "kind:{kind}",
        "kind:{kind} {symbol}",
        "kind:{kind} in {language}",
        "find kind:{kind}",
        "query kind:{kind}",
        "search kind:{kind} {symbol}",
        "{kind} kind symbols",
        "show kind:{kind} results",
        "all kind:{kind}",
        # lang: predicate patterns
        "lang:{language}",
        "lang:{language} {kind}s",
        "find lang:{language} {symbol}",
        "query lang:{language}",
        "{kind}s lang:{language}",
        "in lang:{language}",
        "show lang:{language} symbols",
        "search lang:{language}",
        # visibility: predicate patterns
        "visibility:public",
        "visibility:private",
        "find visibility:public {kind}s",
        "visibility:public in {language}",
        "public visibility symbols",
        "show visibility:private {kind}s",
        "find visibility:public {symbol}",
        "query visibility:public kind:{kind}",
        # async: predicate patterns
        "async:true",
        "async:true {kind}s",
        "find async:true {kind}s in {language}",
        "async functions",
        "async methods",
        "show async:true",
        "query async:true",
        "find async {kind}s",
        # name: predicate patterns
        "name:{symbol}",
        "find name:{symbol}",
        "query name:{symbol}",
        "name:{symbol} kind:{kind}",
        "show name:{symbol}",
        "search name:{symbol}",
        # Boolean operator patterns (AND)
        "kind:{kind} AND name:{symbol}",
        "kind:{kind} AND lang:{language}",
        "lang:{language} AND visibility:public",
        "kind:{kind} AND visibility:public",
        "find kind:{kind} AND name:{symbol}",
        "query kind:{kind} AND lang:{language}",
        "{kind}s AND {symbol}",
        "async:true AND kind:{kind}",
        "visibility:public AND async:true",
        # Boolean operator patterns (OR)
        "kind:function OR kind:method",
        "kind:class OR kind:struct",
        "lang:rust OR lang:go",
        "find kind:{kind} OR kind:struct",
        "query lang:{language} OR lang:python",
        "kind:enum OR kind:type",
        "visibility:public OR visibility:private",
        # Complex boolean queries
        "kind:{kind} AND lang:{language} AND visibility:public",
        "find kind:{kind} AND name:{symbol} in {path}",
        "(kind:function OR kind:method) AND async:true",
        "kind:{kind} AND (lang:rust OR lang:go)",
        # ============================================
        # CD (Cross-File Discovery) PREDICATE PATTERNS
        # ============================================
        # impl: predicate patterns (trait implementations)
        "impl:{symbol}",
        "find impl:{symbol}",
        "query impl:{symbol}",
        "show impl:{symbol}",
        "find implementations of {symbol}",
        "find types implementing {symbol}",
        "find types that implement {symbol}",
        "what implements {symbol}",
        "who implements {symbol}",
        "structs implementing {symbol}",
        "classes implementing {symbol}",
        "show all implementations of {symbol}",
        "impl:{symbol} in {language}",
        "find impl:{symbol} in {path}",
        # duplicates: predicate patterns
        "duplicates:",
        "duplicates:body",
        "duplicates:signature",
        "find duplicates:",
        "query duplicates:",
        "find duplicate code",
        "find duplicate functions",
        "find duplicate {kind}s",
        "show duplicates:",
        "find duplicated code",
        "find code duplication",
        "detect duplicates",
        "find copy-paste code",
        "show duplicate code",
        "duplicates: in {path}",
        "find duplicates: in {language}",
        # circular: predicate patterns
        "circular:",
        "circular:calls",
        "circular:imports",
        "find circular:",
        "query circular:",
        _FIND_CIRCULAR_DEPS,
        "find circular imports",
        "find circular calls",
        "detect cycles",
        "find cycles in code",
        "show circular dependencies",
        "circular: in {path}",
        "find circular: in {language}",
        "circular dependency detection",
        # unused: predicate patterns
        "unused:",
        "unused:true",
        "find unused:",
        "query unused:",
        "find unused code",
        "find unused {kind}s",
        "find unused functions",
        "find unused variables",
        "show unused:",
        "find dead code",
        "detect unused code",
        "unused: in {path}",
        "find unused: in {language}",
        "unused code detection",
        "find unreferenced {kind}s",
        # ============================================
        # PREDICATE-ONLY PATTERNS (no symbol required)
        # These are critical for classifier accuracy
        # ============================================
        # Async-only patterns
        "find async {kind}s",
        "show async {kind}s",
        "list async {kind}s",
        "show all async {kind}s",
        "find all async {kind}s",
        "async {kind}s in {language}",
        "find async {kind}s in {language}",
        "show async {kind}s in {path}",
        "list all async {kind}s",
        "what async {kind}s exist",
        "which {kind}s are async",
        # Unsafe-only patterns
        "find unsafe {kind}s",
        "show unsafe {kind}s",
        "list unsafe {kind}s",
        "show all unsafe {kind}s",
        "find all unsafe {kind}s",
        "unsafe {kind}s in {language}",
        "find unsafe {kind}s in {language}",
        "show unsafe {kind}s in {path}",
        "list all unsafe {kind}s",
        "what unsafe {kind}s exist",
        "which {kind}s are unsafe",
        "find unsafe code",
        "show unsafe code",
        "list unsafe code",
        # Visibility-only patterns (without symbol)
        "find public {kind}s",
        "find private {kind}s",
        "show public {kind}s",
        "show private {kind}s",
        "list public {kind}s",
        "list private {kind}s",
        "public {kind}s in {language}",
        "private {kind}s in {language}",
        "find public {kind}s in {path}",
        "find private {kind}s in {path}",
        "show all public {kind}s",
        "show all private {kind}s",
        "what public {kind}s exist",
        "what private {kind}s exist",
        "which {kind}s are public",
        "which {kind}s are private",
        # Combined predicate patterns (visibility + async/unsafe)
        "find public async {kind}s",
        "find private async {kind}s",
        "find public unsafe {kind}s",
        "find private unsafe {kind}s",
        "show public async {kind}s",
        "show private async {kind}s",
        "public async {kind}s in {language}",
        "private async {kind}s in {language}",
        "find public async {kind}s in {language}",
        "find private unsafe {kind}s in {language}",
        "list public async {kind}s",
        "list private unsafe {kind}s",
        # Combined predicate patterns (visibility + async + language/path)
        "find public async {kind}s in {path}",
        "show private unsafe {kind}s in {path}",
        "public async {kind}s in rust",
        "private unsafe {kind}s in rust",
        # CD predicate-only patterns (no symbol)
        "find duplicates",
        "find duplicate {kind}s",
        "show duplicates",
        _FIND_CIRCULAR_DEPS,
        "show circular dependencies",
        "find unused {kind}s",
        "show unused {kind}s",
        "find dead code",
        "show dead code",
        "find unreferenced {kind}s",
        "show unreferenced {kind}s",
        # Sort options
        "find {symbol} sorted by name",
        "find {symbol} sort by file",
        "show {kind}s sorted by line",
        "find {kind}s sort by kind",
        # Preview/context options
        "find {symbol} with preview",
        "find {symbol} with {limit} lines context",
        "show {kind} with preview",
        "find {symbol} with code context",
        # Qualified name patterns
        "find {symbol} with qualified names",
        "show qualified name for {symbol}",
        "find {symbol}::{symbol}",
        "{symbol}::{symbol} {kind}",
        # Output format patterns (SymbolQuery context)
        "find {symbol} as json",
        "find {kind}s as csv",
        "query kind:{kind} output json",
        "find {symbol} json format",
        "show {kind}s tsv format",
    ],
    "TextSearch": [
        # Basic grep patterns
        "grep for {pattern}",
        "grep {pattern}",
        "grep {pattern} in {language}",
        "search for {pattern}",
        "search {pattern}",
        "look for {pattern}",
        "look for {pattern} in code",
        # Text search patterns
        "search text {pattern}",
        "text search {pattern}",
        "text search for {pattern}",
        "search string {pattern}",
        "find occurrences of {pattern}",
        "find all occurrences of {pattern}",
        # Comment patterns
        "find TODO comments",
        "search for {pattern} in comments",
        "find comments containing {pattern}",
        "search for FIXME",
        "look for FIXME in code",
        # Macro patterns (containing !)
        "find all panic! calls",
        "find println statements",
        "look for println statements",
        "search for assert! macros",
        "find assert! in code",
        # Keyword patterns
        "grep unsafe blocks",
        "find unsafe blocks",
        "search for deprecated annotations",
        "find deprecated code",
        "look for deprecated",
        "find unsafe",
        # Literal patterns
        "find lines containing {pattern}",
        "search for literal {pattern}",
        "find all {pattern}",
        # Security patterns
        "grep for hardcoded passwords",
        "search for API_KEY",
        "find connection string",
        "search for connection string",
        "look for secrets",
        "find hardcoded {pattern}",
        # Import patterns
        "find all imports of {pattern}",
        "search for imports",
        "find imports of {symbol}",
        # Debug patterns
        "grep for debug logs",
        "find debug statements",
        "search for error messages",
        # Config patterns
        "search for localhost",
        "find hardcoded values",
        "find magic numbers",
        "look for magic numbers",
        # Legal patterns
        "find copyright headers",
        "search for license",
        # HARD NEGATIVES: Patterns that look like SymbolQuery but are TextSearch
        # (looking for literal text/patterns in code, not symbols)
        "search for '{pattern}' in code",
        "find text '{pattern}'",
        "grep for '{pattern}'",
        "look for the text {pattern}",
        "search for the string {pattern}",
        "find literal '{pattern}'",
        "grep for the word {pattern}",
        "search for {pattern} pattern",
        "find the {pattern} pattern in code",
        "look for {pattern} string",
        "search for error in comments",
        "find error messages in code",
        "grep for error in logs",
        "search for config in strings",
        "find hardcoded error",
        "look for error text",
        "find {pattern} in string literals",
        "search {pattern} in comments",
        "grep {pattern} in messages",
        "find the word {pattern}",
        "search for mention of {pattern}",
        "look for references to {pattern} in text",
    ],
    "FindCallers": [
        # Basic who/what calls patterns
        "who calls {symbol}",
        "what calls {symbol}",
        "what calls the {symbol} {kind}",
        # Callers patterns
        "callers of {symbol}",
        "show callers of {symbol}",
        "find all callers of {symbol}",
        "callers of the {symbol} {kind}",
        # Usage patterns
        "find usages of {symbol}",
        "find uses of {symbol}",
        "where is {symbol} used",
        "find all references to {symbol}",
        "show all references to {symbol}",
        # Invocation patterns
        "who invokes {symbol}",
        "what invokes {symbol}",
        "who uses the {symbol} method",
        "who uses {symbol}",
        # Reference patterns
        "references to {symbol}",
        "incoming calls to {symbol}",
        "find call sites for {symbol}",
        "where is {symbol} called from",
        # Function caller patterns
        "what functions call {symbol}",
        "show who calls {symbol}",
        # Dependency patterns
        "who depends on {symbol}",
        "what depends on {symbol}",
        # Depth patterns
        "callers of {symbol} with depth {depth}",
        # Qualified patterns
        "show callers of {symbol}::new",
        "callers of {symbol}::{symbol}",
        # ============================================
        # SQRY-SPECIFIC RELATION PREDICATE PATTERNS
        # ============================================
        # callers: predicate syntax
        "callers:{symbol}",
        "query callers:{symbol}",
        "find callers:{symbol}",
        "show callers:{symbol}",
        "callers:{symbol} in {language}",
        "callers:{symbol} depth {depth}",
        "callers:{symbol} with depth {depth}",
        "callers:{symbol} lang:{language}",
        # imports: predicate syntax (who imports)
        "imports:{symbol}",
        "query imports:{symbol}",
        "who imports {symbol}",
        "what imports {symbol}",
        "show imports:{symbol}",
        "imports:{symbol} in {path}",
        "find imports:{symbol}",
        "modules that import {symbol}",
        "files that import {symbol}",
        # Output format patterns
        "callers of {symbol} as json",
        "show callers:{symbol} json",
        "callers:{symbol} csv format",
        "find callers:{symbol} output json",
        "callers:{symbol} --json",
        # Depth/limit patterns
        "callers:{symbol} limit {limit}",
        "callers:{symbol} first {limit}",
        "top {limit} callers of {symbol}",
        "callers:{symbol} max {limit}",
        # Qualified name patterns
        "callers:{symbol}::{symbol}",
        "show callers:{symbol}::new",
        "callers of {symbol}::handle",
        "callers:{symbol} with qualified names",
    ],
    "FindCallees": [
        # Basic what does X call patterns
        "what does {symbol} call",
        "what does {symbol} use",
        "what does {symbol} invoke",
        "what does {symbol} depend on",
        # Callees patterns
        "callees of {symbol}",
        "show callees of {symbol}",
        "find callees of {symbol}",
        # Functions called patterns
        "functions called by {symbol}",
        "{kind}s called by {symbol}",
        "methods called by {symbol}",
        "what methods does {symbol} call",
        "what functions does {symbol} invoke",
        "what functions does {symbol} use",
        # Dependencies patterns
        "dependencies of {symbol}",
        "show dependencies of {symbol}",
        "find dependencies of {symbol}",
        # Outgoing patterns
        "outgoing calls from {symbol}",
        "show outgoing calls from {symbol}",
        "calls made by {symbol}",
        # Invocation patterns
        "functions invoked by {symbol}",
        "show what {symbol} invokes",
        "find functions invoked by {symbol}",
        # Used patterns
        "functions used by {symbol}",
        "{kind}s used by {symbol}",
        # Depth patterns
        "callees of {symbol} with depth {depth}",
        # Qualified patterns
        "callees of {symbol}::handle",
        "what does {symbol}::{symbol} call",
        # ============================================
        # SQRY-SPECIFIC RELATION PREDICATE PATTERNS
        # ============================================
        # callees: predicate syntax
        "callees:{symbol}",
        "query callees:{symbol}",
        "find callees:{symbol}",
        "show callees:{symbol}",
        "callees:{symbol} in {language}",
        "callees:{symbol} depth {depth}",
        "callees:{symbol} with depth {depth}",
        "callees:{symbol} lang:{language}",
        # exports: predicate syntax (what does X export)
        "exports:{symbol}",
        "query exports:{symbol}",
        "what does {symbol} export",
        "show exports:{symbol}",
        "exports:{symbol} in {path}",
        "find exports:{symbol}",
        "symbols exported by {symbol}",
        "module exports from {symbol}",
        "list exports:{symbol}",
        # Output format patterns
        "callees of {symbol} as json",
        "show callees:{symbol} json",
        "callees:{symbol} csv format",
        "find callees:{symbol} output json",
        "callees:{symbol} --json",
        "exports:{symbol} json",
        # Depth/limit patterns
        "callees:{symbol} limit {limit}",
        "callees:{symbol} first {limit}",
        "top {limit} callees of {symbol}",
        "callees:{symbol} max {limit}",
        "callees:{symbol} max depth {depth}",
        # Qualified name patterns
        "callees:{symbol}::{symbol}",
        "show callees:{symbol}::new",
        "callees of {symbol}::init",
        "callees:{symbol} with qualified names",
    ],
    "TracePath": [
        # Basic trace patterns
        "trace from {from_symbol} to {to_symbol}",
        "trace {from_symbol} to {to_symbol}",
        "trace {from_symbol} to {to_symbol} depth {depth}",
        # Path patterns
        "path from {from_symbol} to {to_symbol}",
        "find path {from_symbol} to {to_symbol}",
        "show path from {from_symbol} to {to_symbol}",
        "path between {from_symbol} and {to_symbol}",
        # How does X reach Y patterns (critical for accuracy)
        "how does {from_symbol} reach {to_symbol}",
        "how does {from_symbol} call {to_symbol}",
        "how does {from_symbol} flow to {to_symbol}",
        # Call path patterns
        "call path between {from_symbol} and {to_symbol}",
        "call sequence from {from_symbol} to {to_symbol}",
        "show call chain from {from_symbol} to {to_symbol}",
        "call chain from {from_symbol} to {to_symbol}",
        # Connection patterns
        "connection between {from_symbol} and {to_symbol}",
        "link between {from_symbol} and {to_symbol}",
        # Trace call graph patterns
        "trace call graph from {from_symbol} to {to_symbol}",
        # Depth patterns
        "path with max depth {depth} from {from_symbol} to {to_symbol}",
        "trace with depth {depth} from {from_symbol} to {to_symbol}",
        # Flow patterns
        "how does input flow to output",
        "how does data flow from {from_symbol} to {to_symbol}",
        "trace {from_symbol} flow to {to_symbol}",
        # Authentication/domain patterns
        "trace authentication flow to {to_symbol}",
        "trace user validation to {to_symbol}",
        # ============================================
        # SQRY-SPECIFIC GRAPH TRACE-PATH PATTERNS
        # ============================================
        # graph trace-path command patterns
        "graph trace-path {from_symbol} {to_symbol}",
        "graph trace-path from {from_symbol} to {to_symbol}",
        "sqry graph trace-path {from_symbol} {to_symbol}",
        # Depth option patterns
        "trace-path {from_symbol} to {to_symbol} depth {depth}",
        "path {from_symbol} to {to_symbol} --depth {depth}",
        "trace {from_symbol} to {to_symbol} max depth {depth}",
        "find shortest path {from_symbol} to {to_symbol}",
        "shortest path from {from_symbol} to {to_symbol}",
        # Format output patterns
        "trace {from_symbol} to {to_symbol} as json",
        "path {from_symbol} to {to_symbol} json format",
        "trace-path {from_symbol} {to_symbol} --format json",
        "trace {from_symbol} to {to_symbol} format text",
        "trace {from_symbol} to {to_symbol} as dot",
        "trace {from_symbol} to {to_symbol} as mermaid",
        # Direction patterns
        "trace from {from_symbol} going to {to_symbol}",
        "reverse path from {to_symbol} to {from_symbol}",
        # Call chain depth patterns
        "call chain depth from {from_symbol}",
        "call-chain-depth {symbol}",
        "max call depth for {symbol}",
        "how deep is {symbol} call chain",
        "calculate call depth from {symbol}",
        # Dependency tree patterns
        "dependency tree for {symbol}",
        "dependency-tree {symbol}",
        "show transitive dependencies of {symbol}",
        "all dependencies of {symbol}",
        "recursive dependencies from {symbol}",
        # Combined options
        "trace {from_symbol} to {to_symbol} depth {depth} json",
        "path {from_symbol} to {to_symbol} --depth {depth} --format mermaid",
    ],
    "Visualize": [
        # Basic visualize patterns
        "visualize {symbol}",
        "visualize {symbol} {kind}",
        "visualize dependencies",
        "visualize dependencies of {symbol}",
        "visualize auth flow",
        # Draw patterns
        "draw call graph",
        "draw call graph for {symbol}",
        "draw relationship diagram",
        "draw diagram of {symbol}",
        "draw {symbol} as {format}",
        # Generate patterns
        "generate diagram for {symbol}",
        "generate diagram for module",
        "generate mermaid for {symbol}",
        "generate graph for {symbol}",
        # Create patterns
        "create diagram of {symbol}",
        "create DOT graph",
        "create {relation} visualization for {symbol}",
        "create {format} diagram",
        # Show patterns
        "show {relation} diagram for {symbol}",
        "show graph of {symbol}",
        "show {symbol} call graph",
        "show mermaid diagram",
        "show visual of call hierarchy",
        # Graph patterns
        "graph {symbol}",
        "graph dependencies",
        # Render/export patterns
        "render {symbol} diagram",
        "export {symbol} diagram as {format}",
        "export diagram as {format}",
        # Diagram patterns
        "diagram the {symbol} module",
        "diagram {symbol}",
        # Format-specific patterns
        "{format} diagram for {symbol}",
        "show {format} for {symbol}",
        # ============================================
        # SQRY-SPECIFIC VISUALIZATION PATTERNS
        # ============================================
        # sqry visualize command patterns
        "sqry visualize callers:{symbol}",
        "sqry visualize callees:{symbol}",
        "visualize callers:{symbol}",
        "visualize callees:{symbol}",
        "visualize imports:{symbol}",
        "visualize exports:{symbol}",
        # Format options (mermaid, graphviz, d2)
        "visualize {symbol} --format mermaid",
        "visualize {symbol} --format graphviz",
        "visualize {symbol} --format d2",
        "visualize callers:{symbol} mermaid",
        "visualize callees:{symbol} graphviz",
        "show {symbol} as mermaid diagram",
        "generate d2 for {symbol}",
        "create graphviz for {symbol}",
        # Output options (svg, png, pdf)
        "visualize {symbol} --output svg",
        "visualize {symbol} --output png",
        "visualize {symbol} --output pdf",
        "visualize {symbol} as svg",
        "visualize {symbol} as png image",
        "export {symbol} visualization as svg",
        "render {symbol} diagram as pdf",
        "visualize {symbol} --format mermaid --output svg",
        # Direction options
        "visualize {symbol} --direction top-down",
        "visualize {symbol} --direction left-right",
        "visualize {symbol} top to bottom",
        "visualize {symbol} left to right",
        "visualize {symbol} direction right-left",
        # Depth options
        "visualize {symbol} --depth {depth}",
        "visualize {symbol} depth {depth}",
        "visualize callers:{symbol} with depth {depth}",
        "visualize callees:{symbol} max depth {depth}",
        # Max nodes options
        "visualize {symbol} --max-nodes {limit}",
        "visualize {symbol} max {limit} nodes",
        "visualize callers:{symbol} limit {limit} nodes",
        "show {symbol} diagram max {limit} results",
        # Combined options
        "visualize callers:{symbol} --format mermaid --depth {depth}",
        "visualize callees:{symbol} --format graphviz --output svg",
        "visualize {symbol} mermaid depth {depth} max-nodes {limit}",
        "visualize callers:{symbol} d2 format output png",
        # Graph subcommands
        "graph stats",
        "graph cycles",
        "graph complexity",
        "graph cross-language",
        "show graph stats",
        "detect cycles",
        _FIND_CIRCULAR_DEPS,
        "show cross-language relationships",
        "calculate complexity metrics",
        # Output file patterns
        "visualize {symbol} --output-file diagram.svg",
        "visualize {symbol} save to {path}",
        "export {symbol} diagram to file",
    ],
    "IndexStatus": [
        # Basic status patterns
        "index status",
        "show index status",
        "check index",
        "check index health",
        "index information",
        "show index info",
        # Up to date patterns
        "is the index up to date",
        "is index up to date",
        "is indexing complete",
        # Stats patterns
        "index stats",
        "index statistics",
        "show index statistics",
        # Time patterns
        "when was the index built",
        "when was index last updated",
        # Coverage patterns
        "index coverage",
        "show index coverage",
        "what files are indexed",
        "show indexed files",
        # Language patterns
        "show indexed languages",
        "what languages are indexed",
        # Symbol count patterns
        "how many symbols indexed",
        "how many files indexed",
        "count indexed symbols",
        # Status of patterns
        "status of code index",
        "status of index",
        "reindex status",
        # ============================================
        # SQRY-SPECIFIC INDEX COMMAND PATTERNS
        # ============================================
        # sqry index command patterns
        "sqry index --status",
        "sqry index status",
        "run sqry index --status",
        "check sqry index",
        # Build/rebuild patterns
        "build index",
        "rebuild index",
        "rebuild the index",
        "sqry index",
        "sqry index --force",
        "force rebuild index",
        "index --force",
        "reindex the codebase",
        "index the project",
        # Validation patterns
        "validate index",
        "index validation",
        "check index integrity",
        "index health check",
        "index --validate",
        "is index valid",
        "index corruption check",
        # JSON output patterns
        "index status json",
        "index status as json",
        "sqry index --status --json",
        "show index status json format",
        # Metrics patterns
        "index metrics",
        "index prometheus metrics",
        "index metrics json",
        "sqry index --status --metrics-format prometheus",
        "export index metrics",
        # Threading patterns
        "index with {limit} threads",
        "sqry index --threads {limit}",
        "parallel indexing",
        "single threaded index",
        # Incremental patterns
        "incremental index status",
        "is index incremental",
        "full reindex",
        "index --no-incremental",
        # Cache patterns
        "index cache status",
        "clear index cache",
        "index cache location",
        # Compression patterns
        "is index compressed",
        "index compression status",
        "index --no-compress",
        # Pre-warming patterns
        "prewarm the index",
        "index prewarm",
        "sqry index --prewarm",
        "index --prewarm-mode aggressive",
        "is index prewarmed",
        # Gitignore patterns
        "add index to gitignore",
        "sqry index --add-to-gitignore",
        # Language list patterns
        "what languages does sqry support",
        "list supported languages",
        "sqry --list-languages",
        "show enabled languages",
    ],
    "Ambiguous": [
        # Help patterns
        "help",
        "help me",
        "what can you do",
        "what do you do",
        # Greeting patterns
        "hello",
        "hi",
        "hey",
        # Vague patterns
        "find stuff",
        "search",
        "show me something",
        "do something",
        "code",
        "something",
        "anything",
        "the thing",
        # Incomplete patterns
        "find",
        "show",
        "what",
        "where",
        "functions",
        "more",
        # Single word patterns
        "run",
        "execute",
        "test",
        "it",
        # Response patterns
        "yes",
        "no",
        "ok",
        "please",
        "again",
        # Noise patterns
        "???",
        "huh",
        "123",
        "asdfjkl",
        "...",
        "what is this",
        "find it",
        "show it",
    ],
}


# Slot fillers for template augmentation
SYMBOLS = [
    "authenticate_user",
    "UserAuth",
    "login",
    "handle_request",
    "process_payment",
    "validate_input",
    "DatabaseConnection",
    "HttpClient",
    "parse_json",
    "render_template",
    "main",
    "init",
    "setup",
    "teardown",
    "Config",
    "Logger",
    "Cache",
    "Queue",
    "Worker",
    "Handler",
    "std::collections::HashMap",
    "Vec<String>",
    "Option::unwrap",
    "Result::map",
    "Iterator::collect",
    "pkg.NewClient",
    "http.Server",
    "context.Context",
    "fmt.Println",
    "json.Marshal",
    "React.Component",
    "useState",
    "useEffect",
    "axios.get",
    "express.Router",
]

LANGUAGES = [
    "rust",
    "python",
    "javascript",
    "typescript",
    "go",
    "java",
    "cpp",
    "c",
    "ruby",
    "php",
    "sql",
    "terraform",
]

KINDS = [
    "function",
    "class",
    "struct",
    "enum",
    "method",
    "trait",
    "interface",
    "module",
    "type",
    "constant",
    "variable",
]

# Path patterns for location-based queries
PATHS = [
    "src/",
    "src/api",
    "src/auth",
    "lib/",
    "tests/",
    "settings.rs",
    "config.py",
    "api/handlers",
    "core/",
    "utils/",
]

PATTERNS = [
    "TODO",
    "FIXME",
    "HACK",
    "BUG",
    "XXX",
    "error",
    "warning",
    "deprecated",
    "password",
    "api_key",
    "secret",
    "token",
    "auth",
    "login",
]

RELATIONS = [
    "call",
    "import",
    "export",
    "inherit",
    "impl",
    "callers",
    "callees",
    "dependency",
]

FORMATS = [
    "mermaid",
    "dot",
    "json",
    "graphviz",
    "d2",
    "text",
    "svg",
    "png",
    "pdf",
    "csv",
    "tsv",
]

# Sort options for --sort parameter
SORT_OPTIONS = [
    "file",
    "line",
    "name",
    "kind",
]

# Direction options for visualization
DIRECTIONS = [
    "top-down",
    "bottom-up",
    "left-right",
    "right-left",
]

# Visibility options
VISIBILITIES = [
    "public",
    "private",
    "protected",
]


class TrainingSample(TypedDict):
    """Training sample format."""

    id: str
    text: str
    intent: str
    source: str  # "template" or "augmented"


class DeterministicRng:
    """Deterministic RNG backed by SHA-256 for reproducible sampling."""

    def __init__(self, seed: bytes) -> None:
        self.seed = seed
        self.counter = 0

    @classmethod
    def from_seed(cls, seed: int | None) -> "DeterministicRng":
        if seed is None:
            seed_bytes = secrets.token_bytes(32)
        else:
            seed_bytes = str(seed).encode("utf-8")
        return cls(hashlib.sha256(seed_bytes).digest())

    def _next_bytes(self, count: int) -> bytes:
        chunks = bytearray()
        while len(chunks) < count:
            counter_bytes = self.counter.to_bytes(8, "big")
            chunks.extend(hashlib.sha256(self.seed + counter_bytes).digest())
            self.counter += 1
        return bytes(chunks[:count])

    def randbelow(self, upper: int) -> int:
        if upper <= 0:
            raise ValueError("upper must be positive")
        bits = upper.bit_length()
        bytes_needed = (bits + 7) // 8
        while True:
            value = int.from_bytes(self._next_bytes(bytes_needed), "big")
            value &= (1 << bits) - 1
            if value < upper:
                return value

    def randint(self, start: int, end: int) -> int:
        if end < start:
            raise ValueError("end must be >= start")
        return start + self.randbelow(end - start + 1)

    def choice(self, seq: Sequence[T]) -> T:
        if not seq:
            raise IndexError("cannot choose from an empty sequence")
        return seq[self.randbelow(len(seq))]

    def shuffle(self, items: MutableSequence[T]) -> None:
        for i in range(len(items) - 1, 0, -1):
            j = self.randbelow(i + 1)
            items[i], items[j] = items[j], items[i]

    def sample(self, population: Sequence[T], count: int) -> list[T]:
        if count < 0 or count > len(population):
            raise ValueError("sample larger than population")
        items = list(population)
        self.shuffle(items)
        return items[:count]


def _replace_symbol_tokens(template: str, rng: DeterministicRng) -> str:
    result = template
    if "{symbol}" in result:
        result = result.replace("{symbol}", rng.choice(SYMBOLS))

    from_symbol: str | None = None
    if "{from_symbol}" in result:
        from_symbol = rng.choice(SYMBOLS)
        result = result.replace("{from_symbol}", from_symbol)

    if "{to_symbol}" in result:
        if from_symbol is None:
            to_symbol = rng.choice(SYMBOLS)
        else:
            available = [s for s in SYMBOLS if s != from_symbol]
            to_symbol = rng.choice(available) if available else from_symbol
        result = result.replace("{to_symbol}", to_symbol)

    return result


def _apply_template_replacements(
    template: str,
    replacements: list[tuple[str, Callable[[], str]]],
) -> str:
    result = template
    for token, provider in replacements:
        if token in result:
            result = result.replace(token, provider())
    return result


def fill_template(template: str, rng: DeterministicRng) -> str:
    """Fill a template with random slot values."""
    result = _replace_symbol_tokens(template, rng)

    replacements: list[tuple[str, Callable[[], str]]] = [
        ("{language}", lambda: rng.choice(LANGUAGES)),
        ("{kind}", lambda: rng.choice(KINDS)),
        ("{pattern}", lambda: rng.choice(PATTERNS)),
        ("{relation}", lambda: rng.choice(RELATIONS)),
        ("{format}", lambda: rng.choice(FORMATS)),
        ("{limit}", lambda: str(rng.randint(5, 50))),
        ("{depth}", lambda: str(rng.randint(3, 10))),
        ("{path}", lambda: rng.choice(PATHS)),
        ("{sort}", lambda: rng.choice(SORT_OPTIONS)),
        ("{direction}", lambda: rng.choice(DIRECTIONS)),
        ("{visibility}", lambda: rng.choice(VISIBILITIES)),
    ]

    return _apply_template_replacements(result, replacements)


def augment_text(text: str, rng: DeterministicRng) -> str:
    """Apply random augmentations to text."""
    augmentations = [
        lambda t: t.lower(),
        lambda t: t.upper(),
        lambda t: t.capitalize(),
        lambda t: "please " + t,
        lambda t: t + " please",
        lambda t: "can you " + t,
        lambda t: "I want to " + t,
        lambda t: "I need to " + t,
        lambda t: t + "?",
        lambda t: "show me " + t if not t.startswith("show") else t,
        lambda t: t.replace("find", "search") if "find" in t.lower() else t,
        lambda t: t.replace("search", "find") if "search" in t.lower() else t,
    ]

    # Apply 0-2 random augmentations
    num_augmentations = rng.randint(0, 2)
    for _ in range(num_augmentations):
        aug = rng.choice(augmentations)
        text = aug(text)

    return text


def generate_sample_id(text: str, intent: str, idx: int) -> str:
    """Generate a unique sample ID."""
    content = f"{intent}:{text}:{idx}"
    return hashlib.sha256(content.encode()).hexdigest()[:12]


def build_sample(text: str, intent: str, source: str, idx: int) -> TrainingSample:
    return {
        "id": generate_sample_id(text, intent, idx),
        "text": text,
        "intent": intent,
        "source": source,
    }


def generate_template_samples(
    templates: list[str],
    count: int,
    intent: str,
    source: str,
    rng: DeterministicRng,
    idx: int,
    augment: bool,
) -> tuple[list[TrainingSample], int]:
    samples: list[TrainingSample] = []
    for _ in range(count):
        template = rng.choice(templates)
        text = fill_template(template, rng)
        if augment:
            text = augment_text(text, rng)
        samples.append(build_sample(text, intent, source, idx))
        idx += 1
    return samples, idx


def generate_samples_for_intent(
    intent: str,
    templates: list[str],
    samples_per_intent: int,
    augmentation_ratio: float,
    rng: DeterministicRng,
    idx: int,
) -> tuple[list[TrainingSample], int]:
    base_count = int(samples_per_intent * (1 - augmentation_ratio))
    base_samples, idx = generate_template_samples(
        templates,
        base_count,
        intent,
        "template",
        rng,
        idx,
        augment=False,
    )

    aug_count = samples_per_intent - base_count
    augmented_samples, idx = generate_template_samples(
        templates,
        aug_count,
        intent,
        "augmented",
        rng,
        idx,
        augment=True,
    )

    return base_samples + augmented_samples, idx


def generate_samples(
    samples_per_intent: int,
    augmentation_ratio: float = 0.5,
    seed: int | None = None,
) -> list[TrainingSample]:
    """Generate training samples using templates."""
    rng = DeterministicRng.from_seed(seed)

    samples: list[TrainingSample] = []
    idx = 0

    for intent, templates in INTENT_TEMPLATES.items():
        intent_samples, idx = generate_samples_for_intent(
            intent,
            templates,
            samples_per_intent,
            augmentation_ratio,
            rng,
            idx,
        )
        samples.extend(intent_samples)

    # Shuffle samples
    rng.shuffle(samples)

    return samples


def compute_statistics(samples: list[TrainingSample]) -> dict:
    """Compute dataset statistics."""
    intent_counts: dict[str, int] = {}
    source_counts: dict[str, int] = {}

    for sample in samples:
        intent = sample["intent"]
        source = sample["source"]
        intent_counts[intent] = intent_counts.get(intent, 0) + 1
        source_counts[source] = source_counts.get(source, 0) + 1

    return {
        "total_samples": len(samples),
        "intents": intent_counts,
        "sources": source_counts,
        "unique_texts": len({s["text"] for s in samples}),
    }


def sample_for_verification(
    samples: list[TrainingSample],
    ratio: float = 0.1,
    seed: int = 42,
) -> list[TrainingSample]:
    """Sample data for manual verification (H6 requirement)."""
    rng = DeterministicRng.from_seed(seed)
    count = int(len(samples) * ratio)
    return rng.sample(samples, count)


@app.command()
def generate(
    output: Path = typer.Option(
        Path("data/train.json"),
        help="Output file path",
    ),
    samples_per_intent: int = typer.Option(
        1000,
        help="Number of samples per intent class",
    ),
    augmentation_ratio: float = typer.Option(
        0.5,
        help="Ratio of augmented samples (0-1)",
    ),
    seed: int | None = typer.Option(
        None,
        help="Random seed for reproducibility",
    ),
    verification_output: Path | None = typer.Option(
        None,
        help="Output file for verification samples (10%)",
    ),
) -> None:
    """Generate training data using template-based augmentation."""
    # AC-11.2: Enforce minimum 1000 samples per intent
    if samples_per_intent < MIN_SAMPLES_PER_INTENT:
        console.print(
            f"[red]ERROR: AC-11.2 requires >= {MIN_SAMPLES_PER_INTENT} samples per intent.[/red]"
        )
        console.print(f"[red]Got: {samples_per_intent}. Use --samples-per-intent {MIN_SAMPLES_PER_INTENT} or higher.[/red]")
        raise typer.Exit(1)

    console.print("[bold blue]sqry-nl Training Data Generator[/bold blue]")
    console.print()

    # Ensure output directory exists
    output.parent.mkdir(parents=True, exist_ok=True)

    with Progress(
        SpinnerColumn(),
        TextColumn("[progress.description]{task.description}"),
        console=console,
    ) as progress:
        task = progress.add_task("Generating samples...", total=None)

        samples = generate_samples(
            samples_per_intent=samples_per_intent,
            augmentation_ratio=augmentation_ratio,
            seed=seed,
        )

        progress.update(task, description="Computing statistics...")
        stats = compute_statistics(samples)

        progress.update(task, description="Writing output...")
        with open(output, "w", encoding="utf-8") as f:
            json.dump(
                {
                    "metadata": {
                        "generator": "sqry-nl/training/generate_data.py",
                        "samples_per_intent": samples_per_intent,
                        "augmentation_ratio": augmentation_ratio,
                        "seed": seed,
                        "statistics": stats,
                    },
                    "samples": samples,
                },
                f,
                indent=2,
            )

        # Generate verification sample if requested
        if verification_output is not None:
            progress.update(task, description="Generating verification sample...")
            verification_output.parent.mkdir(parents=True, exist_ok=True)
            verification_samples = sample_for_verification(samples)
            with open(verification_output, "w", encoding="utf-8") as f:
                json.dump(
                    {
                        "metadata": {
                            "purpose": "Manual verification (H6 risk mitigation)",
                            "ratio": 0.1,
                            "count": len(verification_samples),
                        },
                        "samples": verification_samples,
                    },
                    f,
                    indent=2,
                )

    # Print summary
    console.print()
    console.print(f"[green]Generated {stats['total_samples']} samples[/green]")
    console.print(f"[dim]Output: {output}[/dim]")
    console.print()

    console.print("[bold]Intent Distribution:[/bold]")
    for intent, count in sorted(stats["intents"].items()):
        console.print(f"  {intent}: {count}")

    console.print()
    console.print("[bold]Source Distribution:[/bold]")
    for source, count in sorted(stats["sources"].items()):
        console.print(f"  {source}: {count}")

    if verification_output is not None:
        console.print()
        console.print(
            f"[yellow]Verification sample written to: {verification_output}[/yellow]"
        )
        console.print(
            "[dim]IMPORTANT: Manually verify 10% of samples before training (H6)[/dim]"
        )


@app.command()
def verify(
    input_file: Path = typer.Argument(..., help="Verification samples file"),
) -> None:
    """Interactive verification of generated samples (H6 requirement)."""
    with open(input_file, encoding="utf-8") as f:
        data = json.load(f)

    samples = data["samples"]
    console.print(f"[bold]Verifying {len(samples)} samples[/bold]")
    console.print()

    correct = 0
    incorrect = 0
    corrections: list[dict] = []

    for i, sample in enumerate(samples):
        console.print(f"[dim]Sample {i+1}/{len(samples)}[/dim]")
        console.print(f"  Text: [cyan]{sample['text']}[/cyan]")
        console.print(f"  Intent: [yellow]{sample['intent']}[/yellow]")

        response = typer.prompt(
            "Is this correct? (y/n/q to quit)",
            default="y",
        )

        if response.lower() == "q":
            break
        elif response.lower() == "y":
            correct += 1
        else:
            incorrect += 1
            new_intent = typer.prompt(
                f"Enter correct intent {INTENTS}",
                default=sample["intent"],
            )
            corrections.append(
                {
                    "id": sample["id"],
                    "original_intent": sample["intent"],
                    "corrected_intent": new_intent,
                    "text": sample["text"],
                }
            )

        console.print()

    # Summary
    console.print("[bold]Verification Summary:[/bold]")
    console.print(f"  Correct: {correct}")
    console.print(f"  Incorrect: {incorrect}")
    if corrections:
        total_reviewed = correct + incorrect
        if total_reviewed > 0:
            accuracy = correct / total_reviewed * 100
            console.print(f"  Accuracy: {accuracy:.1f}%")
        else:
            console.print("  Accuracy: N/A (no responses)")
        console.print()
        console.print("[bold]Corrections needed:[/bold]")
        for c in corrections:
            console.print(
                f"  {c['id']}: {c['original_intent']} -> {c['corrected_intent']}"
            )


if __name__ == "__main__":
    app()
