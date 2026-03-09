# Design: [Component Name]

🔒 **Status**: Draft
**Created**: YYYY-MM-DD
**Related Spec**: [Link to 01_SPEC.md]

---
## Token Optimization (Required)
Use `docs/TOKEN_OPTIMIZATION_GUIDE.md`.
- Dense phrasing; drop filler/articles when safe.
- Prefer lists/tables; avoid narrative blocks.
- One sentence per bullet; avoid hedging.
- Use snake_case; standard names.
- Compact `{id,name}`; inline `field:type!>0`.


## Approval

**Approval Status**: ⏸️ Awaiting Self-Approval

> Self-approve after review:
> ```markdown
> 🔒 Read-Only (Approved: YYYY-MM-DD by: Your Name)
> Rationale: [Why this design is optimal, trade-offs considered, alignment with 3-layer architecture]
> ```

---

## Architecture Overview

### High-Level Design

```
┌─────────────────────────────────────┐
│         Component Name              │
│                                     │
│  [ASCII diagram showing structure]  │
│                                     │
└─────────────────────────────────────┘
         │              │
         ▼              ▼
   [Subcomponent]  [Subcomponent]
```

**Component responsibilities**:
1. [Responsibility 1]
2. [Responsibility 2]

**Integration points**:
- [ ] Integrates with: [Component A]
- [ ] Consumes: [Component B API]
- [ ] Produces: [Output for Component C]

---

## API Design

### Public Traits

```rust
/// [Brief description]
pub trait ComponentName {
    /// [Doc comment]
    fn primary_operation(&self, input: InputType) -> Result<OutputType>;
    
    /// [Doc comment]
    fn secondary_operation(&mut self, params: Params) -> Result<()>;
}
```

### Public Types

```rust
/// [Description]
pub struct MainType {
    pub field1: Type1,
    pub field2: Type2,
    // Private fields...
}

/// [Description]
pub enum ResultKind {
    Variant1,
    Variant2(Data),
}
```

### Public Functions

```rust
/// [Description]
pub fn create_component(config: Config) -> Result<Box<dyn ComponentName>>;
```

---

## Data Structures

### Internal Structures

```rust
struct InternalState {
    cache: LruCache<Key, Value>,
    index: HashMap<String, Vec<Entry>>,
}
```

### Data Flow

```
[Input] → [Process 1] → [Process 2] → [Output]
            │                │
            ▼                ▼
        [Cache]         [Index]
```

---

## Implementation Strategy

### Core Algorithm

[Pseudocode or detailed description of the main algorithm]

### Caching Strategy

- **Cache what**: [What data to cache]
- **Invalidation**: [When to invalidate]
- **Size limits**: [Max entries, memory limits]

### Error Handling

| Error Type | Handling Strategy | Recovery |
|------------|-------------------|----------|
| [Error 1] | [How detected, what to do] | [How user recovers] |
| [Error 2] | [How detected, what to do] | [How user recovers] |

---

## Plugin Integration (if applicable)

### Plugin Trait Extension

```rust
pub trait LanguagePlugin {
    // Existing methods...
    
    /// [New method this component needs]
    fn new_method(&self) -> Result<Data>;
}
```

### Backward Compatibility

- [ ] Existing plugins work without changes
- [ ] New plugins can opt into new functionality
- [ ] Migration path documented

---

## Alternatives Considered

### Alternative 1: [Name]

**Approach**: [Description]

**Pros**:
- ✅ [Advantage 1]
- ✅ [Advantage 2]

**Cons**:
- ❌ [Disadvantage 1]
- ❌ [Disadvantage 2]

**Decision**: ❌ Rejected because [reason]

---

### Alternative 2: [Name]

[Same structure as above]

---

### Chosen Design: [Name]

**Approach**: [Description]

**Pros**:
- ✅ [Advantage 1]
- ✅ [Advantage 2]

**Cons**:
- ⚠️ [Known limitation 1]
- ⚠️ [Known limitation 2]

**Decision**: ✅ **Selected** because [rationale]

---

## Performance Considerations

### Complexity Analysis

- **Time Complexity**: O([complexity]) for [operation]
- **Space Complexity**: O([complexity]) for [data structure]

### Benchmarks (Planned)

| Operation | Target | Measurement Method |
|-----------|--------|-------------------|
| [Op 1] | [target] | [how to measure] |
| [Op 2] | [target] | [how to measure] |

### Optimization Opportunities

1. [Optimization 1]: [When to apply, expected gain]
2. [Optimization 2]: [When to apply, expected gain]

---

## Testing Strategy

### Unit Tests

- [ ] Test [component A] in isolation
- [ ] Test [edge case 1]
- [ ] Test [error case 1]

### Integration Tests

- [ ] Test interaction with [component B]
- [ ] Test end-to-end workflow

### Property-Based Tests (if applicable)

- [ ] Property: [invariant that must hold]
- [ ] Property: [invariant that must hold]

---

## Migration & Compatibility

### API Stability

- **Version**: [MAJOR/MINOR/PATCH impact]
- **Breaking changes**: [Yes/No - list if yes]
- **Deprecation plan**: [If replacing existing API]

### Migration Guide (if breaking)

**Before**:
```rust
// Old API usage
```

**After**:
```rust
// New API usage
```

---

## Security & Privacy

### Input Validation

- [ ] Validate [input type 1]: [how]
- [ ] Sanitize [input type 2]: [how]

### Resource Limits

- [ ] Max recursion depth: [limit]
- [ ] Max memory usage: [limit]

---

## References

- **Spec**: [Link to 01_SPEC.md]
- **Similar Projects**: [Links to related work]
- **Technical Papers**: [Citations]

---

## Delete Before Approval

**Checklist**:
- [ ] All placeholder text removed
- [ ] Code examples compile
- [ ] Alternatives documented with rationale
- [ ] Performance targets defined
- [ ] Testing strategy clear
- [ ] Aligns with 3-layer architecture
- [ ] Plugin system compatibility verified
- [ ] This section deleted

## Template Instructions (Delete Before Approval)

**How to use this template**:

1. **Fill in all sections** above
2. **Remove all placeholder text** (anything in [brackets])
3. **Delete this "Template Instructions" section**
4. **Verify semantic search litmus test** passes
5. **Self-review** for completeness and clarity
6. **Submit for review to all the available llm providers** iterate until ALL items have been addressed and UNCONDITIONAL approval is granted
7. **Mark as read-only** (🔒 status at top)
