//! Relation extraction for HTML documents.
//!
//! Provides `HtmlGraphBuilder` for extracting HTML resource relationships:
//! - Script imports (`<script src="...">`)
//! - Stylesheet imports (`<link rel="stylesheet" href="...">`)
//! - Image assets (`<img src="...">`)
//! - Media assets (`<video>`, `<audio>`, `<source>`)
//! - Frame references (`<iframe src="...">`)
//! - Module preloads (`<link rel="modulepreload">`)
//!
//! No new semantics here. New behaviour must go via `sqry_core::graph::GraphBuilder` and the language-specific `*GraphBuilder` (see this module's export) to build `CodeGraph`.

mod graph_builder;

pub use graph_builder::HtmlGraphBuilder;
