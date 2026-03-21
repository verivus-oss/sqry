# Cross-Language Example Fixture

This test fixture demonstrates sqry's cross-language analysis capabilities with a realistic multi-language application.

## Structure

```
cross-language-example/
├── frontend/       # JavaScript frontend
│   └── api.js      # HTTP calls to backend + FFI to native
├── backend/        # Python backend
│   └── server.py   # Flask API + FFI to native
├── native/         # C++ native library
│   └── lib.cpp     # Compression, hashing, auth validation
└── expected/       # Expected sqry output
    ├── graph.dot   # Expected graph visualization
    └── trace.txt   # Expected trace output examples
```

## Languages

- **JavaScript** (frontend): Makes HTTP calls to Python backend, FFI calls to C++ library
- **Python** (backend): Flask API server, FFI calls to C++ library
- **C++** (native): Shared library for compression, hashing, authentication

## Cross-Language Edges

### HTTP Calls (JavaScript → Python)

1. `fetchUsers()` → `GET /api/users` → `get_users()`
2. `createUser()` → `POST /api/users` → `create_user()`

### FFI Calls

#### JavaScript → C++
- `compressData()` → `compress()` (via node-ffi)

#### Python → C++
- `authenticate_request()` → `validate_token()` (via ctypes)
- `create_user()` → `hash_password()` (via ctypes)

## Use Cases Demonstrated

### 1. Full-Stack Tracing

Trace a request from frontend to database:

```bash
sqry trace "main" -> "save_to_database"
```

**Expected**: 7-hop path through JavaScript → HTTP → Python

### 2. Security Audit

Find all paths to database writes:

```bash
sqry trace "*" -> "save_to_database"
```

**Expected**: Should find `createUser` path

### 3. Cross-Language Visualization

Visualize the entire call graph:

```bash
sqry visualize --format dot graph.dot
dot -Tsvg graph.dot -o graph.svg
```

**Expected**: Should match `expected/graph.dot`

### 4. HTTP Endpoint Discovery

Find all HTTP calls:

```bash
sqry graph --cross-lang --filter "HTTPRequest"
```

**Expected**: 2 HTTP calls (GET, POST to /api/users)

### 5. FFI Call Analysis

Find all FFI boundaries:

```bash
sqry graph --cross-lang --filter "FFICall"
```

**Expected**: 3 FFI calls (compress, validate_token, hash_password)

## Testing Commands

Run these commands to verify sqry's cross-language analysis:

```bash
# Navigate to fixture directory
cd test-fixtures/cross-language-example

# Index the fixture
sqry index

# Test 1: Find all callers of database write
sqry graph --callers "save_to_database"
# Expected: create_user

# Test 2: Trace from frontend to backend
sqry trace "main" -> "get_users"
# Expected: 4 hops (main -> fetchUsers -> HTTP -> get_users)

# Test 3: Find HTTP endpoints
sqry graph --cross-lang --format json | jq '.edges[] | select(.kind == "HTTPRequest")'
# Expected: 2 edges (GET and POST)

# Test 4: Find FFI calls
sqry graph --cross-lang --format json | jq '.edges[] | select(.kind == "FFICall")'
# Expected: 3 edges (compress, validate_token, hash_password)

# Test 5: Full graph visualization
sqry visualize --format dot graph.dot
diff graph.dot expected/graph.dot
# Expected: Should match (modulo ordering)
```

## Validation Criteria

This fixture should demonstrate:

- ✅ Cross-language call detection (JavaScript → Python via HTTP)
- ✅ FFI call detection (JavaScript → C++, Python → C++)
- ✅ Multi-hop path tracing across language boundaries
- ✅ HTTP endpoint identification and routing
- ✅ Confidence scoring for detected edges
- ✅ Visualization of cross-language dependencies

## Expected Metrics

| Metric | Expected Value |
|--------|----------------|
| Total nodes | 15 |
| JavaScript nodes | 4 |
| Python nodes | 6 |
| C++ nodes | 5 |
| HTTP edges | 2 |
| FFI edges | 3 |
| Regular call edges | ~10 |
| Max path length (main → save_to_database) | 7 hops |

## Notes

- This is a **mock fixture** - the code doesn't actually run
- Purpose is to test sqry's **static analysis** capabilities
- All cross-language edges should be detected via static analysis (no runtime needed)
- HTTP endpoints detected by analyzing axios/fetch calls and Flask routes
- FFI calls detected by analyzing `require()` and `import native_lib` patterns

## Integration Test Usage

This fixture can be used in integration tests:

```rust
#[test]
fn test_cross_language_http_detection() {
    let graph = index_fixture("cross-language-example");

    // Find HTTP edges
    let http_edges = graph.edges_by_kind(EdgeKind::HTTPRequest);
    assert_eq!(http_edges.len(), 2);

    // Verify endpoints
    assert!(http_edges.iter().any(|e| e.endpoint == "/api/users" && e.method == "GET"));
    assert!(http_edges.iter().any(|e| e.endpoint == "/api/users" && e.method == "POST"));
}

#[test]
fn test_cross_language_ffi_detection() {
    let graph = index_fixture("cross-language-example");

    // Find FFI edges
    let ffi_edges = graph.edges_by_kind(EdgeKind::FFICall);
    assert_eq!(ffi_edges.len(), 3);

    // Verify targets
    let targets: Vec<_> = ffi_edges.iter().map(|e| &e.to.qualified_name).collect();
    assert!(targets.contains(&"compress"));
    assert!(targets.contains(&"validate_token"));
    assert!(targets.contains(&"hash_password"));
}

#[test]
fn test_full_stack_trace() {
    let graph = index_fixture("cross-language-example");

    // Trace from main to database
    let path = graph.trace_path("main", "save_to_database");
    assert!(path.is_some());

    let path = path.unwrap();
    assert_eq!(path.len(), 7); // 7 hops

    // Verify crosses language boundaries
    let languages: Vec<_> = path.iter().map(|n| n.language).collect();
    assert!(languages.contains(&Language::JavaScript));
    assert!(languages.contains(&Language::Python));
}
```

## Future Extensions

This fixture could be extended to demonstrate:

- WebSocket connections (JavaScript ↔ Python)
- gRPC calls (Python → Go)
- Database queries (Python → SQL)
- Message queue operations (Python → RabbitMQ → Python)
- Microservice communication (REST, GraphQL)
