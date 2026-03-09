# TypeScript Relation Fixtures

Curated fixtures that drive the TypeScript relation smoke tests.

## Files

- `calls.ts` – chained calls, constructor invocations, `this` usage, namespace access
- `imports.ts` – default, named, namespace, and type-only imports (including aliases)
- `exports.ts` – named exports, default class export, type exports, and re-export wiring
- `returns.ts` – functions returning `Promise<T>`, async functions, and arrow return types

Each fixture is intentionally small while touching the language constructs used by
the relation extractor. The smoke tests compare the extracted edges against golden
expectations for determinism.
