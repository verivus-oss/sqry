# Search for Symbols

sqry lets you find any symbol in your workspace by name — across all 35 supported languages.

## How to Search

- **Keyboard shortcut**: `Ctrl+Alt+S` (Mac: `Cmd+Alt+S`)
- **Command Palette**: `Ctrl+Shift+P` → `Sqry: Search Workspace`
- **Sidebar**: Click the search icon in the Sqry panel

Type a name and sqry returns matching functions, classes, methods, constants, types, and more.

## Example Searches

| Query | Finds |
|-------|-------|
| `parse` | All symbols containing "parse" in their name |
| `handleRequest` | The exact function `handleRequest` and close matches |
| `UserService` | The class `UserService` across all files and languages |
| `validate` | Every `validate` function, method, or variable |

## What You See

Results appear in the **Sqry Semantic Results** sidebar panel. Each result shows:

- Symbol name and kind (function, class, method, etc.)
- File path and line number
- Language

Click any result to jump directly to its definition.

## Result Limit

By default sqry returns up to 200 results. Adjust this with the `sqry.limit` setting
if you need more (or fewer) results per query.

## Tip: Use Structured Queries for Precision

Name search is great for quick lookups. For more targeted searches — filtering by kind,
language, visibility, or relationships — use **Run Query** (`Ctrl+Alt+Q`) instead.
