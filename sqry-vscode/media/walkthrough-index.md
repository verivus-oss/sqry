# Index Your Workspace

Before you can search, sqry needs to build a semantic index of your code.

## What Indexing Does

sqry parses every source file in your workspace using tree-sitter, a fast incremental
parser that understands 35 programming languages. It extracts:

- **Symbols**: functions, classes, structs, traits, interfaces, constants, types
- **Relationships**: call edges, inheritance, imports/exports, FFI boundaries
- **Metadata**: visibility, language, file location, parameter counts

All of this is stored as a unified graph — enabling queries that go far beyond
plain text search.

## How to Index

Use any of these methods:

- **Command Palette**: `Ctrl+Shift+P` → `Sqry: Index Workspace`
- **Keyboard shortcut**: `Ctrl+Alt+I` (Mac: `Cmd+Alt+I`)
- **Sidebar button**: Click the index button in the Sqry panel

## Auto-Index on Open

sqry can index automatically when you open a workspace. Control this with the
`sqry.autoIndexOnOpen` setting:

- `prompt` (default) — asks before indexing
- `always` — indexes silently on every open
- `never` — never auto-indexes

## Index Storage

The index is stored in `.sqry/graph/snapshot.sqry` inside your workspace root.
This directory is created automatically. You may want to add `.sqry/` to your `.gitignore`.

## Index Time

- Small projects (< 10k files): a few seconds
- Medium projects (10k–100k files): 15–60 seconds
- Large monorepos (100k+ files): 1–5 minutes

Subsequent indexes are faster because sqry only processes changed files.
