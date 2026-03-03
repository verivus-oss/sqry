# MCP Expected Responses Fixtures

These fixtures capture MCP tool responses for parity testing.

Notes on `kind` casing:
- `get_document_symbols` responses use PascalCase `kind` values (e.g., `Function`, `Module`).
- `semantic_search` and `hierarchical_search` responses use lowercase `kind` values (e.g., `function`).

This reflects current MCP tool serialization behavior; do not normalize casing in fixtures.
