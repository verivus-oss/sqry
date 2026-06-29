//! [A2 §K] Gate 0b coverage-matrix CI exhaustiveness test.
//!
//! This is the third leg of the four-legged exhaustiveness regime from
//! the plan (see
//! `docs/superpowers/plans/2026-03-19-sqryd-daemon.md`, Step 0b, under
//! "What `assert_impl_all!` does NOT guarantee" — the other three legs
//! are the code-owner rule, the PR template checklist, and the §E
//! equivalence harness).
//!
//! The test enforces five distinct drift surfaces, each with its own
//! dedicated check:
//!
//! 1. **Plan §K `assert_impl_all!` code block ↔ `coverage.rs`**: the set of
//!    types listed in the plan's rust fenced code block must equal the set
//!    of types with live `assert_impl_all!(<T>: NodeIdBearing);` lines in
//!    `coverage.rs`.
//! 2. **`coverage.rs` is the only file asserting the trait**: no other file
//!    in `sqry-core/src` may invoke `assert_impl_all!` on `NodeIdBearing`,
//!    or the grep above can be fooled by a secondary location.
//! 3. **Field-identity classification**: every field on the real
//!    `CodeGraph` struct (parsed by **name**, not by type) must be classified
//!    as either bearing (listed in plan K.A / active K.B by field name) or
//!    excluded (listed in the plan's "Other `CodeGraph` fields that are
//!    intentionally NOT in K.A" bullet block). A field whose type happens
//!    to match an already-classified bearing type — e.g. a second
//!    `Arc<AliasTable>` — still fails classification because its **name**
//!    is not in the plan.
//! 4. **Plan exclusion bullet list ↔ real struct**: the set of field names
//!    in the plan's exclusion paragraph must equal the set of fields the
//!    classifier marks excluded. Dropping a bullet from the plan while the
//!    field still lives on `CodeGraph` fails CI; adding a stale bullet for
//!    a field that no longer exists also fails CI.
//! 5. **Plan K.A field-on-`CodeGraph` column ↔ real struct**: every field
//!    name the plan's K.A table claims to cover must actually exist on
//!    `CodeGraph`, and conversely every classified-bearing `CodeGraph`
//!    field must be named by at least one K.A or active K.B row. Same
//!    contract in both directions as #4, applied to the bearing half.
//!
//! Synthetic drift tests exercise each of these on in-memory fixtures so
//! regressions in the classifier or parser surface deterministically,
//! without touching the real files on disk.
//!
//! The test reads the repository on disk relative to `CARGO_MANIFEST_DIR`
//! so it runs identically under `cargo test -p sqry-core --test
//! gate0b_coverage_matrix` and `cargo test --workspace`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Locate the repository root by walking up from `sqry-core/Cargo.toml`.
fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points to `.../sqry-core` during tests.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .expect("sqry-core must live in a workspace")
        .to_path_buf()
}

// ---------------------------------------------------------------------------
// Surface 1: assert_impl_all! parser (shared between plan and coverage.rs).
// ---------------------------------------------------------------------------

/// Extract every `<Type>` referenced by an uncommented
/// `assert_impl_all!(<Type>: NodeIdBearing);` line in the given source
/// text. Commented lines (`// assert_impl_all!(...)`) are skipped —
/// this mirrors the plan's explicit "K.B entries (added as tasks land)"
/// policy, where a commented assertion denotes a reserved future row.
fn parse_assert_impl_all_types(source: &str) -> BTreeSet<String> {
    source
        .lines()
        .filter_map(|raw| {
            let line = raw.trim_start();
            // Skip commented-out assertions (reserved future rows).
            if line.starts_with("//") {
                return None;
            }
            // Match `assert_impl_all!(Type: NodeIdBearing)`. We look for
            // the trait name to avoid false positives if another
            // assert_impl_all! for a different trait is ever added.
            let open = line.find("assert_impl_all!(")?;
            let rest = &line[open + "assert_impl_all!(".len()..];
            let close = rest.find(')')?;
            let inner = &rest[..close];
            // `inner` looks like `NodeArena: NodeIdBearing`.
            let (ty, trait_part) = inner.split_once(':')?;
            if trait_part.trim() != "NodeIdBearing" {
                return None;
            }
            Some(ty.trim().to_string())
        })
        .collect()
}

#[test]
fn coverage_rs_matches_plan_assert_impl_all_block() {
    let root = repo_root();

    let coverage_path = root.join("sqry-core/src/graph/unified/rebuild/coverage.rs");
    let coverage_src = fs::read_to_string(&coverage_path).unwrap_or_else(|e| {
        panic!(
            "coverage.rs must exist at {} (err: {e})",
            coverage_path.display()
        )
    });

    let plan_path = root.join("docs/superpowers/plans/2026-03-19-sqryd-daemon.md");
    let plan_src = fs::read_to_string(&plan_path).unwrap_or_else(|e| {
        panic!(
            "sqryd daemon plan must exist at {} (err: {e})",
            plan_path.display()
        )
    });

    let coverage_types = parse_assert_impl_all_types(&coverage_src);
    assert!(
        !coverage_types.is_empty(),
        "coverage.rs must contain at least one assert_impl_all!(<Type>: NodeIdBearing); \
         line — found none. Did the file get accidentally truncated?"
    );

    let plan_block = extract_plan_assert_block(&plan_src).unwrap_or_else(|| {
        panic!(
            "Could not locate the § K `assert_impl_all!` block in the plan at {}. \
             Expected a fenced rust code block containing \
             `assert_impl_all!(<Type>: NodeIdBearing);` lines inside the \
             §K — or §`Step 0b` — section.",
            plan_path.display()
        )
    });
    let plan_types = parse_assert_impl_all_types(&plan_block);
    assert!(
        !plan_types.is_empty(),
        "Plan §K assert_impl_all! block parsed to an empty type set. Block contents:\n{plan_block}"
    );

    if coverage_types != plan_types {
        let only_in_coverage: BTreeSet<_> = coverage_types.difference(&plan_types).collect();
        let only_in_plan: BTreeSet<_> = plan_types.difference(&coverage_types).collect();
        panic!(
            "Gate 0b coverage-matrix drift between plan §K and coverage.rs.\n\n\
             Types present in coverage.rs but NOT in plan §K (add row to plan or remove impl): {only_in_coverage:?}\n\n\
             Types present in plan §K but NOT in coverage.rs (add `assert_impl_all!` entry and impl): {only_in_plan:?}\n\n\
             Plan block parsed at {}:\n{plan_block}\n\n\
             coverage.rs parsed at {}:\n{coverage_types:?}\n",
            plan_path.display(),
            coverage_path.display()
        );
    }
}

/// Extract the `assert_impl_all!` rust block under Step 0b in the plan.
///
/// The plan encodes the coverage matrix in two tables (K.A and K.B) plus
/// a rust fenced code block whose contents are what `coverage.rs` must
/// mirror. This helper finds the `Step 0b` heading and returns the
/// contents of the first rust fenced block after that heading that
/// contains any `assert_impl_all!` line.
fn extract_plan_assert_block(plan_src: &str) -> Option<String> {
    // The plan uses either `### **Step 0b:` (bold) or `- [ ] **Step 0b:`.
    // We anchor on the literal string `Step 0b` to be robust.
    let anchor = plan_src.find("Step 0b")?;
    let rest = &plan_src[anchor..];

    let mut in_block = false;
    let mut buf = String::new();

    for line in rest.lines() {
        let trimmed = line.trim_start();
        if !in_block {
            if trimmed.starts_with("```rust") || trimmed.starts_with("``` rust") {
                in_block = true;
                buf.clear();
            }
            continue;
        }
        // in_block == true
        if trimmed.starts_with("```") {
            // End of a fenced block. If it contained assert_impl_all!, we're done.
            if buf.contains("assert_impl_all!") {
                return Some(buf);
            }
            // Otherwise keep scanning for the next rust block.
            in_block = false;
            buf.clear();
            continue;
        }
        buf.push_str(line);
        buf.push('\n');
    }
    None
}

#[test]
fn plan_block_parser_extracts_rust_fenced_block_containing_assert_impl_all() {
    // Exercises the helper on a minimal synthetic fixture so regressions
    // in the parser are caught independently of plan edits.
    let fixture = "Step 0b: intro text\n\
        ```rust\n\
        // some other rust block without the assertion\n\
        let x = 1;\n\
        ```\n\n\
        Some prose.\n\n\
        ```rust\n\
        use static_assertions::assert_impl_all;\n\
        assert_impl_all!(FooType: NodeIdBearing);\n\
        ```\n";
    let extracted = extract_plan_assert_block(fixture).expect("block must be found");
    assert!(extracted.contains("assert_impl_all!(FooType: NodeIdBearing);"));
    let types = parse_assert_impl_all_types(&extracted);
    assert_eq!(types, BTreeSet::from(["FooType".to_string()]));
}

#[test]
fn parse_assert_impl_all_types_ignores_commented_rows() {
    let src = "\
        // assert_impl_all!(CommentedOut: NodeIdBearing);\n\
        assert_impl_all!(LiveRow: NodeIdBearing);\n\
        ";
    let types = parse_assert_impl_all_types(src);
    assert_eq!(types, BTreeSet::from(["LiveRow".to_string()]));
}

#[test]
fn parse_assert_impl_all_types_ignores_wrong_trait() {
    // If a future assertion uses a different trait it must not be
    // misattributed to the NodeIdBearing matrix.
    let src = "assert_impl_all!(UnrelatedType: SomeOtherTrait);\n";
    let types = parse_assert_impl_all_types(src);
    assert!(types.is_empty());
}

/// Simulated negative test — asserts that if a row were removed from
/// `coverage.rs` while staying in the plan, [`coverage_rs_matches_plan_assert_impl_all_block`]
/// would fail. We exercise this via the helper on a synthetic pair of
/// strings rather than mutating the real files, which would be a
/// destructive side-effect for other tests running in parallel.
#[test]
fn drift_detection_fires_when_coverage_is_missing_a_plan_row() {
    let plan_block = "\
        use static_assertions::assert_impl_all;\n\
        assert_impl_all!(A: NodeIdBearing);\n\
        assert_impl_all!(B: NodeIdBearing);\n\
        ";
    let coverage_block = "\
        use static_assertions::assert_impl_all;\n\
        assert_impl_all!(A: NodeIdBearing);\n\
        // assert_impl_all!(B: NodeIdBearing); // forgot to wire up\n\
        ";
    let plan = parse_assert_impl_all_types(plan_block);
    let cov = parse_assert_impl_all_types(coverage_block);
    assert_ne!(
        plan, cov,
        "the drift-detection symmetric-difference check must fire when coverage.rs \
         is missing a plan row"
    );
    let only_in_plan: BTreeSet<_> = plan.difference(&cov).collect();
    assert_eq!(only_in_plan, BTreeSet::from([&"B".to_string()]));
}

/// Dual direction: a row present in `coverage.rs` but missing from the
/// plan also triggers drift detection (guards against ad-hoc impls
/// landing without plan updates).
#[test]
fn drift_detection_fires_when_plan_is_missing_a_coverage_row() {
    let plan_block = "\
        use static_assertions::assert_impl_all;\n\
        assert_impl_all!(A: NodeIdBearing);\n\
        ";
    let coverage_block = "\
        use static_assertions::assert_impl_all;\n\
        assert_impl_all!(A: NodeIdBearing);\n\
        assert_impl_all!(UnplannedRow: NodeIdBearing);\n\
        ";
    let plan = parse_assert_impl_all_types(plan_block);
    let cov = parse_assert_impl_all_types(coverage_block);
    assert_ne!(plan, cov);
    let only_in_cov: BTreeSet<_> = cov.difference(&plan).collect();
    assert_eq!(only_in_cov, BTreeSet::from([&"UnplannedRow".to_string()]));
}

/// Sanity-check that `coverage.rs` lists the K.A + active K.B row set
/// the plan's §K table describes in prose: eight K.A types (NodeArena /
/// BidirectionalEdgeStore / AuxiliaryIndices / NodeMetadataStore /
/// NodeProvenanceStore / ScopeArena / AliasTable / ShadowTable) plus the
/// single active K.B1 type (FileRegistry).
///
/// This is a belt-and-suspenders check in addition to the plan-vs-coverage
/// diff above: if someone edits the plan's code block to remove a K.A
/// row, both checks must agree that the matrix is broken.
#[test]
fn coverage_rs_contains_all_known_k_a_and_active_k_b_types() {
    let root = repo_root();
    let coverage_path = root.join("sqry-core/src/graph/unified/rebuild/coverage.rs");
    let coverage_src = fs::read_to_string(&coverage_path).expect("coverage.rs readable");
    let types = parse_assert_impl_all_types(&coverage_src);
    let expected: BTreeSet<String> = [
        "NodeArena",
        "BidirectionalEdgeStore",
        "AuxiliaryIndices",
        "NodeMetadataStore",
        "NodeProvenanceStore",
        "ScopeArena",
        "AliasTable",
        "ShadowTable",
        "FileRegistry",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    assert_eq!(
        types, expected,
        "coverage.rs `assert_impl_all!` set must exactly equal the K.A (8) + active K.B (1) row set. \
         If a row is being added or removed, update both the plan §K table and the prose in the \
         `NodeIdBearing` module docs."
    );
}

/// Defensive check: the `coverage.rs` file must be the only location in
/// `sqry-core` that invokes `assert_impl_all!` on `NodeIdBearing`. If
/// other modules ever start asserting the trait, the CI drift check
/// above can be fooled into passing because it only reads one file.
#[test]
fn no_other_file_asserts_node_id_bearing() {
    let root = repo_root();
    let src_root = root.join("sqry-core/src");
    let mut offenders: Vec<PathBuf> = Vec::new();
    walk(&src_root, &mut offenders);

    let canonical = root.join("sqry-core/src/graph/unified/rebuild/coverage.rs");

    let offenders: Vec<&Path> = offenders
        .iter()
        .filter(|p| **p != canonical)
        .map(PathBuf::as_path)
        .collect();
    assert!(
        offenders.is_empty(),
        "assert_impl_all!(..: NodeIdBearing) found outside coverage.rs: {offenders:?} — \
         consolidate the coverage matrix in coverage.rs to keep CI drift-detection sound."
    );
}

// ---------------------------------------------------------------------------
// Surface 2: plan-paragraph parsers (K.A "Field on CodeGraph" + "Other
// fields intentionally NOT in K.A" + K.B "Added by" → field-name sets).
// ---------------------------------------------------------------------------

/// Parse the bullet list under the plan's
/// `**Other \`CodeGraph\` fields that are intentionally NOT in K.A.**`
/// paragraph into a set of field names.
///
/// The plan represents each excluded field as a markdown bullet of the form
/// `- `name: Type` — justification.`. A single bullet may cover multiple
/// fields by listing them comma-separated before the first `—` (em-dash),
/// e.g. `- `fact_epoch: u64`, `epoch: u64`, `confidence: HashMap<...>` —
/// scalars`. Both shapes are supported.
///
/// Returns the ordered-unique set of field names. Unknown / malformed
/// bullets are ignored so the parser degrades safely; the full-surface
/// drift check below catches any resulting divergence from the real
/// struct.
fn parse_plan_not_in_ka_fields(plan_src: &str) -> BTreeSet<String> {
    // Anchor on the paragraph text. Use a distinctive literal so minor
    // heading-style edits to the surrounding text do not misfire the
    // parser.
    let anchor_needle = "Other `CodeGraph` fields that are intentionally NOT in K.A";
    let Some(anchor) = plan_src.find(anchor_needle) else {
        return BTreeSet::new();
    };
    let rest = &plan_src[anchor..];

    let mut fields: BTreeSet<String> = BTreeSet::new();
    // Consume lines until we hit a blank line or the next bold/table/heading marker
    // (the bullet block is terminated by the "If any of those fields ever gains..."
    // prose paragraph, which does not start with `-`).
    let mut saw_first_bullet = false;
    for line in rest.lines() {
        let trimmed = line.trim();
        if let Some(bullet) = trimmed.strip_prefix("- ") {
            saw_first_bullet = true;
            // Everything before the first em-dash (`—`) or hyphen-space (` - `)
            // is the field-declaration list. We conservatively split on `—`.
            let decl = match bullet.find('—') {
                Some(idx) => &bullet[..idx],
                // If no em-dash is present, fall back to the whole bullet
                // (safer than silently dropping the bullet).
                None => bullet,
            };
            // Each comma-separated token looks like "`name: Type`" or
            // "`name: Type<With,Commas>`". We must respect angle-bracket
            // depth when splitting on commas.
            for tok in split_comma_respecting_generics(decl) {
                let tok = tok.trim();
                // Strip surrounding backticks.
                let stripped = tok.trim_matches('`').trim();
                // Take the identifier up to ':' as the field name.
                let Some((name, _rest)) = stripped.split_once(':') else {
                    continue;
                };
                let name = name.trim();
                if name.is_empty() || !is_ident(name) {
                    continue;
                }
                fields.insert(name.to_string());
            }
        } else if saw_first_bullet && trimmed.is_empty() {
            // End of the bullet block.
            break;
        } else if saw_first_bullet && !trimmed.starts_with('-') {
            // Any non-bullet non-empty line after the first bullet also
            // ends the block (defensive).
            break;
        }
    }
    fields
}

/// Parse the plan's K.A markdown table's `Field on `CodeGraph`` column
/// into a set of field names.
///
/// The plan's K.A table has the column ordering
/// `| # | Structure | Field on \`CodeGraph\` | Compaction method |`.
/// Rows whose field column references a nested field (e.g.
/// `indices.kind_index`) are reduced to the top-level `CodeGraph` field
/// name (`indices`), since `CodeGraph` only exposes the parent field.
/// Rows whose field column includes parenthetical annotations
/// (e.g. `` `edges` (forward adjacency) ``) are reduced to the backtick-wrapped
/// identifier.
///
/// Rows whose field column is prose (e.g. `derived from `edges``) — i.e.
/// K.A9 CSR adjacency — do **not** introduce a new field and are
/// filtered out by the `is_ident` check after backtick-stripping.
fn parse_plan_ka_field_names(plan_src: &str) -> BTreeSet<String> {
    parse_plan_table_column_field_names(plan_src, "K.A", "Field on `CodeGraph`")
}

/// Parse the plan's K.B table `Added by` text by pulling out the field-name
/// reference (e.g. `FileRegistry.per_file_nodes` → `files` is not directly
/// recoverable, so we use an explicit mapping). Currently only one K.B
/// row (`B1`) is active, and it is tied to the `files` field via the
/// `FileRegistry` struct. Future K.B rows must extend this mapping
/// explicitly when they land.
///
/// Returns the (possibly empty) set of `CodeGraph` field names covered
/// by active K.B rows.
fn parse_plan_kb_active_field_names(plan_src: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    // K.B1 is active as of Gate 0b: plan body mentions
    // `FileRegistry.per_file_nodes`. Map by the authoritative link
    // between K.B1 and the `files: Arc<FileRegistry>` field on CodeGraph.
    //
    // The mapping is intentionally hard-coded — the plan's K.B column
    // shape is "Structure | Added by | Compaction method", not "Field
    // on CodeGraph", so there is no column text to parse. When a new
    // K.B row becomes active, both this mapping and the plan prose
    // ("same commit") must be updated.
    if plan_src.contains("`FileRegistry.per_file_nodes`") {
        out.insert("files".to_string());
    }
    out
}

/// Parse a markdown pipe-delimited table under a heading containing
/// `anchor_heading` and return the set of cell values in the column
/// whose header matches `column_title`. Intended for small markdown
/// tables such as the plan's K.A row block.
fn parse_plan_table_column_field_names(
    plan_src: &str,
    anchor_heading: &str,
    column_title: &str,
) -> BTreeSet<String> {
    let Some(anchor) = plan_src.find(anchor_heading) else {
        return BTreeSet::new();
    };
    let rest = &plan_src[anchor..];

    // Find the table header row (starts with `| ` and contains column_title).
    let mut header_col_idx: Option<usize> = None;
    let mut in_rows = false;
    let mut out = BTreeSet::new();

    for line in rest.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('|') {
            if in_rows {
                // Table ended.
                break;
            }
            continue;
        }
        let cells: Vec<&str> = trimmed
            .trim_matches('|')
            .split('|')
            .map(str::trim)
            .collect();
        if header_col_idx.is_none() {
            // Look for the header row.
            if let Some(idx) = cells.iter().position(|c| c.contains(column_title)) {
                header_col_idx = Some(idx);
            }
            continue;
        }
        // Skip the markdown separator row `|---|---|...`.
        if cells.iter().all(|c| {
            c.chars()
                .all(|ch| ch == '-' || ch == ':' || ch.is_whitespace())
        }) {
            continue;
        }
        in_rows = true;
        let Some(idx) = header_col_idx else { break };
        let Some(cell) = cells.get(idx) else { continue };
        let cell = cell.trim();
        // The cell may contain a backtick-wrapped identifier plus
        // parenthetical annotations, e.g. `` `edges` (forward adjacency) ``
        // or `` `indices.kind_index` ``. Take the first backtick-delimited
        // substring and reduce to the top-level field (split on `.`).
        let Some(start) = cell.find('`') else {
            continue;
        };
        let rest_cell = &cell[start + 1..];
        let Some(end) = rest_cell.find('`') else {
            continue;
        };
        let ident_span = &rest_cell[..end];
        let top = ident_span.split('.').next().unwrap_or("").trim();
        if !top.is_empty() && is_ident(top) {
            out.insert(top.to_string());
        }
    }
    out
}

/// Return whether `s` is a valid Rust identifier (ASCII underscore-camel).
fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Split `s` on ASCII commas that are outside `<...>` (angle-bracket)
/// depth. Used so that declarations such as `confidence:
/// HashMap<String, ConfidenceMetadata>` are not split inside the generic
/// parameter list.
fn split_comma_respecting_generics(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth: i32 = 0;
    let mut buf = String::new();
    for ch in s.chars() {
        match ch {
            '<' => {
                depth += 1;
                buf.push(ch);
            }
            '>' => {
                depth -= 1;
                buf.push(ch);
            }
            ',' if depth == 0 => {
                out.push(std::mem::take(&mut buf));
            }
            _ => buf.push(ch),
        }
    }
    if !buf.trim().is_empty() {
        out.push(buf);
    }
    out
}

#[test]
fn plan_not_in_ka_fields_parses_real_plan() {
    let root = repo_root();
    let plan_path = root.join("docs/superpowers/plans/2026-03-19-sqryd-daemon.md");
    let plan_src = fs::read_to_string(&plan_path).expect("plan readable");
    let got = parse_plan_not_in_ka_fields(&plan_src);
    let expected: BTreeSet<String> = [
        "strings",
        "edge_provenance",
        "scope_provenance_store",
        "file_segments",
        // Phase A U09 — `c_indirect_tables: Option<CIndirectSideTables>`
        // joins the exclusion list because the inner maps key on
        // NodeId/StringId/FileId but do NOT carry NodeId-bearing payloads
        // onto the publish boundary. See plan §K "Other fields NOT in
        // K.A" for the documented justification.
        "c_indirect_tables",
        "fact_epoch",
        "epoch",
        "confidence",
        // Go T1 implements-and-promotion (Cluster A) — build-time scratch
        // side-channel that the Go plugin populates and the post-
        // Phase-4e pass drains before Pass 5; never reaches publish with
        // un-drained NodeId hints. See 02_DESIGN §3.2 + §6.
        "go_hints",
        // Issue #394 Slice A: R3 definition-signal marker. A scalar bool that
        // records whether the graph carries genuine NodeEntry.is_definition
        // data; never carries a NodeId payload. See plan §K exclusion bullet.
        "definition_signal_present",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    assert_eq!(
        got, expected,
        "Parser output for the plan's 'Other fields intentionally NOT in K.A' bullet list \
         must equal the authoritative set. If the plan block is being reshaped, update both \
         the plan and this expected set."
    );
}

#[test]
fn plan_not_in_ka_fields_synthetic_multi_field_bullet() {
    // The plan uses a single bullet to cover three scalar fields. The
    // parser must recognise all three (fact_epoch, epoch, confidence).
    let fixture = "\
Other `CodeGraph` fields that are intentionally NOT in K.A. intro prose.

- `strings: Arc<StringInterner>` — keyed by `StringId`.
- `fact_epoch: u64`, `epoch: u64`, `confidence: HashMap<String, _>` — scalars.

If any of those fields ever gains a NodeId-bearing payload, a new K.A row must be added.
";
    let got = parse_plan_not_in_ka_fields(fixture);
    let expected: BTreeSet<String> = ["strings", "fact_epoch", "epoch", "confidence"]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    assert_eq!(got, expected);
}

#[test]
fn plan_ka_field_names_parses_real_plan() {
    let root = repo_root();
    let plan_path = root.join("docs/superpowers/plans/2026-03-19-sqryd-daemon.md");
    let plan_src = fs::read_to_string(&plan_path).expect("plan readable");
    let got = parse_plan_ka_field_names(&plan_src);
    // Every CodeGraph field mentioned in K.A's Field-on-CodeGraph column.
    // K.A9 (CSR adjacency) has no field column entry — the cell reads
    // "derived from `edges`" and is already covered by K.A2/K.A3.
    let expected: BTreeSet<String> = [
        "nodes",
        "edges",
        "indices",
        "macro_metadata",
        "node_provenance",
        "scope_arena",
        "alias_table",
        "shadow_table",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    assert_eq!(
        got, expected,
        "Parser output for plan K.A 'Field on `CodeGraph`' column must equal the \
         authoritative set. K.A9 is intentionally absent (CSR is derived; \
         covered by edges)."
    );
}

// ---------------------------------------------------------------------------
// Surface 3: field-identity classifier.
// ---------------------------------------------------------------------------

/// How a single `CodeGraph` field was classified by the Gate 0b matrix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldClassification {
    /// Field name appears in plan K.A or active K.B, **and** its payload
    /// type also has an `assert_impl_all!(T: NodeIdBearing)` in
    /// `coverage.rs`. A bearing field that is missing from `coverage.rs`
    /// is flagged as `Unknown` (the type-name side is still required to
    /// catch the "row exists in plan but impl is missing" subcase).
    Bearing,
    /// Field name appears in the plan's "Other fields intentionally NOT
    /// in K.A" bullet list.
    Excluded,
    /// Field name is neither bearing nor excluded → drift. The reporter
    /// field carries the reason (missing from plan / type unclassified
    /// / both).
    Unknown(String),
}

/// Classify every `CodeGraph` field by **name** into
/// [`FieldClassification`] buckets given:
///
/// * `fields`: the ordered `(field_name, canonicalized_type)` list
///   parsed from `concurrent/graph.rs` by [`parse_code_graph_field_types`].
/// * `plan_ka_fields`: the set of field names in plan K.A's
///   "Field on `CodeGraph`" column, parsed by [`parse_plan_ka_field_names`].
/// * `plan_kb_fields`: the set of field names covered by active plan
///   K.B rows, parsed by [`parse_plan_kb_active_field_names`].
/// * `plan_not_in_ka`: the set of field names in the plan's exclusion
///   bullet list, parsed by [`parse_plan_not_in_ka_fields`].
/// * `coverage_types`: the set of types with `assert_impl_all!` lines
///   in `coverage.rs`, parsed by [`parse_assert_impl_all_types`].
///
/// A field's classification is `Bearing` iff its name is in
/// `plan_ka_fields` ∪ `plan_kb_fields` and its canonicalized type is in
/// `coverage_types`. It is `Excluded` iff its name is in
/// `plan_not_in_ka`. Being in both sets is itself an error
/// (`Unknown("classified as both bearing and excluded")`).
///
/// This function reads **no files** — all inputs are passed in by the
/// caller. The synthetic drift tests exercise it on injected fixtures
/// to prove the classifier catches new-field drift even when the new
/// field reuses an already-classified bearing type.
fn classify_code_graph_fields(
    fields: &[(String, String)],
    plan_ka_fields: &BTreeSet<String>,
    plan_kb_fields: &BTreeSet<String>,
    plan_not_in_ka: &BTreeSet<String>,
    coverage_types: &BTreeSet<String>,
) -> BTreeMap<String, FieldClassification> {
    let mut out: BTreeMap<String, FieldClassification> = BTreeMap::new();
    for (name, ty) in fields {
        let bearing_named = plan_ka_fields.contains(name) || plan_kb_fields.contains(name);
        let excluded_named = plan_not_in_ka.contains(name);
        let type_has_impl = coverage_types.contains(ty);

        if bearing_named && excluded_named {
            out.insert(
                name.clone(),
                FieldClassification::Unknown(format!(
                    "field `{name}` appears in BOTH the plan K.A/K.B field list AND the \
                     'Other fields intentionally NOT in K.A' bullet list — the two \
                     are mutually exclusive by design"
                )),
            );
            continue;
        }

        if bearing_named {
            if type_has_impl {
                out.insert(name.clone(), FieldClassification::Bearing);
            } else {
                out.insert(
                    name.clone(),
                    FieldClassification::Unknown(format!(
                        "field `{name}: {ty}` is listed in plan K.A/K.B as bearing but its \
                         type `{ty}` does not appear in coverage.rs `assert_impl_all!` — \
                         add an `impl NodeIdBearing for {ty}` and a corresponding \
                         `assert_impl_all!({ty}: NodeIdBearing);` line"
                    )),
                );
            }
            continue;
        }

        if excluded_named {
            out.insert(name.clone(), FieldClassification::Excluded);
            continue;
        }

        // Neither bearing nor excluded → drift.
        out.insert(
            name.clone(),
            FieldClassification::Unknown(format!(
                "field `{name}: {ty}` is not named in plan K.A, active K.B, or the \
                 'Other fields intentionally NOT in K.A' bullet list — add a K.A/K.B row \
                 (if NodeId-bearing) OR an exclusion bullet (if not)"
            )),
        );
    }
    out
}

/// Full-surface drift check: every `CodeGraph` field must be classified
/// as bearing or excluded by **name**, not by type. A field whose type
/// happens to match an already-classified bearing type still fails if
/// the plan doesn't name it.
///
/// This is the leg the reviewer's iter-2 blocker flagged as missing: a
/// second `Arc<AliasTable>` field used to be accepted because
/// `AliasTable` was already in `bearing_types`, even though Gate 0c
/// would need another compaction call and the plan would need another
/// row. The classifier below closes that gap by keying on field
/// identity.
#[test]
fn every_code_graph_field_is_classified_by_name() {
    let root = repo_root();
    let graph_path = root.join("sqry-core/src/graph/unified/concurrent/graph.rs");
    let graph_src = fs::read_to_string(&graph_path).expect("concurrent/graph.rs readable");
    let fields = parse_code_graph_field_types(&graph_src);
    assert!(
        !fields.is_empty(),
        "Failed to parse any fields from CodeGraph at {}. \
         Has the struct layout changed?",
        graph_path.display()
    );

    let plan_path = root.join("docs/superpowers/plans/2026-03-19-sqryd-daemon.md");
    let plan_src = fs::read_to_string(&plan_path).expect("plan readable");
    let plan_ka = parse_plan_ka_field_names(&plan_src);
    let plan_kb = parse_plan_kb_active_field_names(&plan_src);
    let plan_not_in_ka = parse_plan_not_in_ka_fields(&plan_src);

    let coverage_path = root.join("sqry-core/src/graph/unified/rebuild/coverage.rs");
    let coverage_src = fs::read_to_string(&coverage_path).expect("coverage.rs readable");
    let coverage_types = parse_assert_impl_all_types(&coverage_src);

    let classifications = classify_code_graph_fields(
        &fields,
        &plan_ka,
        &plan_kb,
        &plan_not_in_ka,
        &coverage_types,
    );

    let unknown: Vec<(String, String)> = classifications
        .iter()
        .filter_map(|(name, kind)| match kind {
            FieldClassification::Unknown(reason) => Some((name.clone(), reason.clone())),
            _ => None,
        })
        .collect();

    assert!(
        unknown.is_empty(),
        "Gate 0b field-identity drift detected on `CodeGraph`:\n{}\n\n\
         Plan K.A field names parsed: {:?}\n\
         Plan active K.B field names parsed: {:?}\n\
         Plan 'Other fields NOT in K.A' names parsed: {:?}\n\
         coverage.rs assert_impl_all! types: {:?}\n",
        unknown
            .iter()
            .map(|(n, r)| format!("  - {n}: {r}"))
            .collect::<Vec<_>>()
            .join("\n"),
        plan_ka,
        plan_kb,
        plan_not_in_ka,
        coverage_types,
    );
}

/// Belt-and-suspenders: the plan's exclusion bullet list must equal the
/// actual set of `CodeGraph` fields the classifier marks as
/// `Excluded`. Deleting a bullet while the field still lives on
/// `CodeGraph`, or adding a stale bullet for a field that no longer
/// exists, both fail CI.
#[test]
fn plan_not_in_ka_matches_classified_excluded_fields() {
    let root = repo_root();
    let graph_path = root.join("sqry-core/src/graph/unified/concurrent/graph.rs");
    let graph_src = fs::read_to_string(&graph_path).expect("concurrent/graph.rs readable");
    let fields = parse_code_graph_field_types(&graph_src);

    let plan_path = root.join("docs/superpowers/plans/2026-03-19-sqryd-daemon.md");
    let plan_src = fs::read_to_string(&plan_path).expect("plan readable");
    let plan_ka = parse_plan_ka_field_names(&plan_src);
    let plan_kb = parse_plan_kb_active_field_names(&plan_src);
    let plan_not_in_ka = parse_plan_not_in_ka_fields(&plan_src);

    let coverage_path = root.join("sqry-core/src/graph/unified/rebuild/coverage.rs");
    let coverage_src = fs::read_to_string(&coverage_path).expect("coverage.rs readable");
    let coverage_types = parse_assert_impl_all_types(&coverage_src);

    let classifications = classify_code_graph_fields(
        &fields,
        &plan_ka,
        &plan_kb,
        &plan_not_in_ka,
        &coverage_types,
    );

    let classifier_excluded: BTreeSet<String> = classifications
        .iter()
        .filter_map(|(name, kind)| match kind {
            FieldClassification::Excluded => Some(name.clone()),
            _ => None,
        })
        .collect();

    // The plan bullets name all excluded fields; the classifier agrees
    // iff every plan bullet points at a real struct field and every
    // real excluded field is bulleted.
    let only_in_plan: BTreeSet<_> = plan_not_in_ka.difference(&classifier_excluded).collect();
    let only_in_struct: BTreeSet<_> = classifier_excluded.difference(&plan_not_in_ka).collect();

    assert!(
        only_in_plan.is_empty() && only_in_struct.is_empty(),
        "Plan 'Other fields intentionally NOT in K.A' bullet list must exactly equal \
         the real CodeGraph fields classified as excluded.\n\n\
         In plan bullets but not on struct (stale bullet — remove): {only_in_plan:?}\n\
         On struct but not in plan bullets (missing bullet — add): {only_in_struct:?}\n"
    );
}

/// Belt-and-suspenders (bearing side): every field name listed in plan
/// K.A / active K.B must actually exist on `CodeGraph`. Rename drift
/// (e.g., renaming `files` to `file_registry` without updating the plan)
/// therefore surfaces here even if the classifier's type-side impl
/// check happens to pass via a different field.
#[test]
fn plan_ka_and_kb_field_names_all_exist_on_code_graph() {
    let root = repo_root();
    let graph_path = root.join("sqry-core/src/graph/unified/concurrent/graph.rs");
    let graph_src = fs::read_to_string(&graph_path).expect("concurrent/graph.rs readable");
    let fields = parse_code_graph_field_types(&graph_src);
    let struct_names: BTreeSet<String> = fields.iter().map(|(n, _)| n.clone()).collect();

    let plan_path = root.join("docs/superpowers/plans/2026-03-19-sqryd-daemon.md");
    let plan_src = fs::read_to_string(&plan_path).expect("plan readable");
    let plan_ka = parse_plan_ka_field_names(&plan_src);
    let plan_kb = parse_plan_kb_active_field_names(&plan_src);

    let missing_ka: BTreeSet<_> = plan_ka.difference(&struct_names).collect();
    assert!(
        missing_ka.is_empty(),
        "Plan K.A references field name(s) not on CodeGraph: {missing_ka:?}. \
         Either rename the struct field back, or update plan K.A and the classifier \
         mapping together."
    );

    let missing_kb: BTreeSet<_> = plan_kb.difference(&struct_names).collect();
    assert!(
        missing_kb.is_empty(),
        "Plan active K.B references field name(s) not on CodeGraph: {missing_kb:?}."
    );
}

// ---------------------------------------------------------------------------
// Synthetic drift tests — prove the classifier would actually fail on
// new-field drift, including the iter-2 reviewer's failure mode where a
// new field reuses an already-classified bearing type.
// ---------------------------------------------------------------------------

/// Aggregated synthetic-fixture state used by the drift tests so we can
/// mutate one surface at a time without re-plumbing every closure
/// argument.
struct BaselineFixture {
    fields: Vec<(String, String)>,
    plan_ka: BTreeSet<String>,
    plan_kb: BTreeSet<String>,
    plan_not_in_ka: BTreeSet<String>,
    coverage_types: BTreeSet<String>,
}

/// Canonical fixture mirroring today's (Gate 0b) `CodeGraph`. Synthetic
/// drift tests mutate this fixture — not the real file — to prove the
/// classifier reacts to each drift shape.
fn baseline_fixture() -> BaselineFixture {
    let fields = vec![
        ("nodes".into(), "NodeArena".into()),
        ("edges".into(), "BidirectionalEdgeStore".into()),
        ("strings".into(), "StringInterner".into()),
        ("files".into(), "FileRegistry".into()),
        ("indices".into(), "AuxiliaryIndices".into()),
        ("macro_metadata".into(), "NodeMetadataStore".into()),
        ("node_provenance".into(), "NodeProvenanceStore".into()),
        ("edge_provenance".into(), "EdgeProvenanceStore".into()),
        ("fact_epoch".into(), "u64".into()),
        ("epoch".into(), "u64".into()),
        (
            "confidence".into(),
            "HashMap<String, ConfidenceMetadata>".into(),
        ),
        ("scope_arena".into(), "ScopeArena".into()),
        ("alias_table".into(), "AliasTable".into()),
        ("shadow_table".into(), "ShadowTable".into()),
        (
            "scope_provenance_store".into(),
            "ScopeProvenanceStore".into(),
        ),
        ("file_segments".into(), "FileSegmentTable".into()),
    ];
    let plan_ka: BTreeSet<String> = [
        "nodes",
        "edges",
        "indices",
        "macro_metadata",
        "node_provenance",
        "scope_arena",
        "alias_table",
        "shadow_table",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    let plan_kb: BTreeSet<String> = ["files"].iter().map(|s| (*s).to_string()).collect();
    let plan_not_in_ka: BTreeSet<String> = [
        "strings",
        "edge_provenance",
        "scope_provenance_store",
        "file_segments",
        "fact_epoch",
        "epoch",
        "confidence",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    let coverage_types: BTreeSet<String> = [
        "NodeArena",
        "BidirectionalEdgeStore",
        "AuxiliaryIndices",
        "NodeMetadataStore",
        "NodeProvenanceStore",
        "ScopeArena",
        "AliasTable",
        "ShadowTable",
        "FileRegistry",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    BaselineFixture {
        fields,
        plan_ka,
        plan_kb,
        plan_not_in_ka,
        coverage_types,
    }
}

impl BaselineFixture {
    fn classify(&self) -> BTreeMap<String, FieldClassification> {
        classify_code_graph_fields(
            &self.fields,
            &self.plan_ka,
            &self.plan_kb,
            &self.plan_not_in_ka,
            &self.coverage_types,
        )
    }
}

#[test]
fn synthetic_baseline_classifies_every_real_field_cleanly() {
    // Sanity: with no drift, every field classifies cleanly.
    let fx = baseline_fixture();
    let got = fx.classify();
    let unknown: Vec<_> = got
        .iter()
        .filter(|(_, k)| matches!(k, FieldClassification::Unknown(_)))
        .map(|(n, _)| n.clone())
        .collect();
    assert!(
        unknown.is_empty(),
        "baseline fixture must classify every field cleanly, got Unknown: {unknown:?}"
    );
}

/// The iter-2 reviewer's failure mode: a **second** field whose type is
/// already classified as bearing (`Arc<AliasTable>`) must still be
/// flagged, because the plan has no row for the new field name.
#[test]
fn synthetic_drift_new_field_reusing_bearing_type_is_caught() {
    let mut fx = baseline_fixture();
    fx.fields
        .push(("alias_table_2".into(), "AliasTable".into()));

    let got = fx.classify();

    match got.get("alias_table_2") {
        Some(FieldClassification::Unknown(msg)) => {
            assert!(
                msg.contains("not named in plan K.A"),
                "unexpected Unknown reason for alias_table_2: {msg}"
            );
        }
        other => panic!(
            "alias_table_2 (new field reusing AliasTable) must be classified as Unknown; \
             got {other:?}. This is the exact drift shape the iter-2 reviewer called out: \
             type-name-only classification was hiding new-field drift."
        ),
    }
}

/// A new field whose type is **not** already classified must also be
/// caught — this is the plain-vanilla drift shape that the iter-1 fix
/// already handled, re-asserted here to guard against regression when
/// the classifier is refactored.
#[test]
fn synthetic_drift_new_field_with_new_type_is_caught() {
    let mut fx = baseline_fixture();
    fx.fields
        .push(("brand_new_table".into(), "BrandNewBearingStore".into()));
    let got = fx.classify();
    match got.get("brand_new_table") {
        Some(FieldClassification::Unknown(_)) => {}
        other => panic!("brand_new_table must be Unknown; got {other:?}"),
    }
}

/// An excluded-looking field that isn't bulleted in the plan must be
/// caught. This guards the "excluded" half of the classifier.
#[test]
fn synthetic_drift_new_unclassified_scalar_field_is_caught() {
    let mut fx = baseline_fixture();
    fx.fields.push(("rebuild_generation".into(), "u64".into()));
    let got = fx.classify();
    match got.get("rebuild_generation") {
        Some(FieldClassification::Unknown(_)) => {}
        other => panic!("rebuild_generation must be Unknown; got {other:?}"),
    }
}

/// If a plan K.A row names a field but the corresponding type has no
/// `assert_impl_all!` entry in `coverage.rs`, the classifier must
/// report Unknown.
#[test]
fn synthetic_drift_plan_row_without_coverage_impl_is_caught() {
    let mut fx = baseline_fixture();
    // Remove the `AliasTable` impl so the bearing name is present but
    // the type-side impl is missing.
    fx.coverage_types.remove("AliasTable");
    let got = fx.classify();
    match got.get("alias_table") {
        Some(FieldClassification::Unknown(msg)) => {
            assert!(
                msg.contains("does not appear in coverage.rs"),
                "expected coverage-side error message, got: {msg}"
            );
        }
        other => panic!("alias_table must be Unknown when impl is missing; got {other:?}"),
    }
}

/// If the plan lists a field as both bearing and excluded (a
/// contradiction), the classifier must surface Unknown with the
/// bi-listed reason.
#[test]
fn synthetic_drift_field_listed_as_both_bearing_and_excluded_is_caught() {
    let mut fx = baseline_fixture();
    // Put `strings` in both lists.
    fx.plan_ka.insert("strings".to_string());
    fx.plan_not_in_ka.insert("strings".to_string());
    let got = fx.classify();
    match got.get("strings") {
        Some(FieldClassification::Unknown(msg)) => {
            assert!(
                msg.contains("appears in BOTH"),
                "expected double-classification error, got: {msg}"
            );
        }
        other => panic!("strings must be Unknown when bi-classified; got {other:?}"),
    }
}

/// Removing a bullet from the plan's "Other fields intentionally NOT in
/// K.A" list while the field still exists on `CodeGraph` must surface
/// as an unclassified field (caught by the classifier) AND as a
/// mismatch between plan bullets and classifier-Excluded set. We
/// exercise the classifier half here.
#[test]
fn synthetic_drift_plan_bullet_removed_while_field_remains_is_caught() {
    let mut fx = baseline_fixture();
    // Drop the `strings` bullet.
    fx.plan_not_in_ka.remove("strings");
    let got = fx.classify();
    match got.get("strings") {
        Some(FieldClassification::Unknown(msg)) => {
            assert!(
                msg.contains("not named in plan"),
                "expected 'not named in plan' error, got: {msg}"
            );
        }
        other => panic!("strings must be Unknown when bullet is removed; got {other:?}"),
    }
}

/// Parse `CodeGraph`'s field declarations out of `concurrent/graph.rs`.
///
/// Returns `(field_name, type_name)` tuples with the type stripped of its
/// `Arc<...>` wrapper where present so the type name matches what
/// `assert_impl_all!(T: NodeIdBearing)` asserts. Scalar and non-generic
/// types are returned verbatim.
fn parse_code_graph_field_types(src: &str) -> Vec<(String, String)> {
    let mut fields = Vec::new();
    let anchor = "pub struct CodeGraph {";
    let Some(start) = src.find(anchor) else {
        return fields;
    };
    let after = &src[start + anchor.len()..];
    let Some(end_off) = after.find("\n}") else {
        return fields;
    };
    let body = &after[..end_off];

    for raw in body.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("//") || line.starts_with("///") {
            continue;
        }
        // Strip the trailing comma and any trailing `// ...` line comment.
        let line = line.trim_end_matches(',');
        let line = match line.find("//") {
            Some(idx) => line[..idx].trim_end(),
            None => line,
        };
        // Shape: `pub field_name: Arc<Type>,` or `field_name: Type,`
        let Some((name_part, type_part)) = line.split_once(':') else {
            continue;
        };
        // Strip leading visibility modifiers (`pub`, `pub(crate)`, etc.).
        let name = name_part
            .split_whitespace()
            .next_back()
            .expect("non-empty name side")
            .to_string();
        let type_str = type_part.trim();
        let canonical = canonicalize_type(type_str);
        fields.push((name, canonical));
    }
    fields
}

/// Strip the `Arc<...>` wrapper from a type name so a declaration like
/// `Arc<NodeArena>` canonicalizes to `NodeArena`. Leaves non-`Arc`
/// declarations (e.g., `u64`, `HashMap<String, ConfidenceMetadata>`)
/// untouched.
fn canonicalize_type(ty: &str) -> String {
    let ty = ty.trim();
    if let Some(rest) = ty.strip_prefix("Arc<")
        && let Some(inner) = rest.strip_suffix('>')
    {
        return inner.trim().to_string();
    }
    ty.to_string()
}

#[test]
fn parse_code_graph_field_types_handles_arc_wrapping() {
    let fixture = "pub struct CodeGraph {\n    \
        /// doc\n    \
        nodes: Arc<NodeArena>,\n    \
        edges: Arc<BidirectionalEdgeStore>,\n    \
        fact_epoch: u64,\n    \
        confidence: HashMap<String, ConfidenceMetadata>,\n\
        }\n";
    let fields = parse_code_graph_field_types(fixture);
    assert_eq!(
        fields,
        vec![
            ("nodes".into(), "NodeArena".into()),
            ("edges".into(), "BidirectionalEdgeStore".into()),
            ("fact_epoch".into(), "u64".into()),
            (
                "confidence".into(),
                "HashMap<String, ConfidenceMetadata>".into()
            ),
        ]
    );
}

#[test]
fn split_comma_respecting_generics_handles_nested() {
    let v = split_comma_respecting_generics(
        "`strings: Arc<StringInterner>`, `confidence: HashMap<String, ConfidenceMetadata>`",
    );
    assert_eq!(v.len(), 2);
    assert!(v[0].contains("StringInterner"));
    assert!(v[1].contains("HashMap<String, ConfidenceMetadata>"));
}

// ---------------------------------------------------------------------------
// Surface 4: residue-helper K-row drift guard (Gate 0d iter-1 Major 2).
//
// `CodeGraph::assert_no_tombstone_residue_for` (concurrent/graph.rs) and
// `RebuildGraph::assert_no_tombstone_residue` (rebuild/rebuild_graph.rs)
// MUST iterate the same K-row set. Without a machine check, a future
// K-row addition can land on one helper and not the other, silently
// regressing the §F.2 residue-containment contract.
//
// The test below parses both function bodies via `syn`, extracts the
// set of fields each `for nid in self.<field>.all_node_ids()` loop
// names, and asserts the two sets are identical. It also cross-checks
// that both sets equal the K-row set the plan + coverage.rs describe,
// so adding a row in `coverage.rs` without extending either residue
// helper fails CI deterministically.
// ---------------------------------------------------------------------------

use syn::visit::{self, Visit};
use syn::{Expr, File as SynFile, ImplItem, Item};

/// Collect the set of `self.<field>` identifiers iterated by
/// `for ... in self.<field>.all_node_ids()` loops inside `fn_body`.
/// Any other loop pattern is ignored; the caller's contract is that
/// every residue-iteration loop in the target helper uses this exact
/// pattern.
struct ResidueFieldVisitor {
    fields: BTreeSet<String>,
}

impl<'ast> Visit<'ast> for ResidueFieldVisitor {
    fn visit_expr_for_loop(&mut self, node: &'ast syn::ExprForLoop) {
        if let Some(field) = extract_self_field_on_all_node_ids(&node.expr) {
            self.fields.insert(field);
        }
        // Continue walking in case nested for-loops also iterate
        // NodeIdBearing fields (none do today, but a future change
        // would still be covered).
        visit::visit_expr_for_loop(self, node);
    }
}

/// If `expr` is the `self.<field>.all_node_ids()` method-call chain,
/// return the field name. Otherwise return `None`.
fn extract_self_field_on_all_node_ids(expr: &Expr) -> Option<String> {
    // Shape: MethodCall { receiver: FieldAccess { base: self, member: <field> }, method: all_node_ids, args: [] }
    let Expr::MethodCall(mc) = expr else {
        return None;
    };
    if mc.method != "all_node_ids" || !mc.args.is_empty() {
        return None;
    }
    let Expr::Field(field) = mc.receiver.as_ref() else {
        return None;
    };
    // Base must be `self` (a `Path` with a single `self` segment).
    let Expr::Path(path) = field.base.as_ref() else {
        return None;
    };
    let is_self = path
        .path
        .segments
        .last()
        .map(|s| s.ident == "self")
        .unwrap_or(false);
    if !is_self {
        return None;
    }
    match &field.member {
        syn::Member::Named(ident) => Some(ident.to_string()),
        syn::Member::Unnamed(_) => None,
    }
}

/// Locate a function by name inside a parsed Rust file, scanning both
/// free functions and `impl` blocks. Returns the function's block
/// (body) if found.
fn find_fn_body<'a>(file: &'a SynFile, fn_name: &str) -> Option<&'a syn::Block> {
    for item in &file.items {
        match item {
            Item::Fn(f) if f.sig.ident == fn_name => {
                return Some(&f.block);
            }
            Item::Impl(imp) => {
                for impl_item in &imp.items {
                    if let ImplItem::Fn(m) = impl_item
                        && m.sig.ident == fn_name
                    {
                        return Some(&m.block);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// Parse the given Rust source and return the set of K-row fields
/// iterated inside the named function body.
fn parse_residue_helper_k_row_fields(src: &str, fn_name: &str) -> BTreeSet<String> {
    let file: SynFile = syn::parse_file(src)
        .unwrap_or_else(|e| panic!("syn failed to parse source for fn `{fn_name}`: {e}"));
    let block = find_fn_body(&file, fn_name)
        .unwrap_or_else(|| panic!("function `{fn_name}` not found in parsed source"));
    let mut visitor = ResidueFieldVisitor {
        fields: BTreeSet::new(),
    };
    visit::visit_block(&mut visitor, block);
    visitor.fields
}

/// Gate 0d iter-1 Major 2 — the residue-helper drift guard.
///
/// Parses the K-row fields iterated by
/// `CodeGraph::assert_no_tombstone_residue_for` and
/// `RebuildGraph::assert_no_tombstone_residue` via `syn` (NOT regex —
/// syntactic parsing is immune to cosmetic reformatting) and asserts:
///
/// 1. Both helpers iterate the **same set of `self.<field>` names**.
/// 2. That set equals the canonical K-row field set implied by the
///    plan's K.A + active K.B row tables (the nine fields `nodes`,
///    `indices`, `edges`, `macro_metadata`, `node_provenance`,
///    `scope_arena`, `alias_table`, `shadow_table`, `files`).
///
/// A future K-row addition therefore **must** update both helpers
/// AND the plan — any two-of-three change fails this test.
#[test]
fn every_k_row_is_covered_by_both_residue_helpers() {
    let root = repo_root();

    let code_graph_path = root.join("sqry-core/src/graph/unified/concurrent/graph.rs");
    let code_graph_src =
        fs::read_to_string(&code_graph_path).expect("concurrent/graph.rs readable");
    let code_graph_fields =
        parse_residue_helper_k_row_fields(&code_graph_src, "assert_no_tombstone_residue_for");

    let rebuild_graph_path = root.join("sqry-core/src/graph/unified/rebuild/rebuild_graph.rs");
    let rebuild_graph_src =
        fs::read_to_string(&rebuild_graph_path).expect("rebuild/rebuild_graph.rs readable");
    let rebuild_graph_fields =
        parse_residue_helper_k_row_fields(&rebuild_graph_src, "assert_no_tombstone_residue");

    // Canonical K-row field set — matches plan K.A1..K.A8 + active K.B1.
    let expected: BTreeSet<String> = [
        "nodes",
        "indices",
        "edges",
        "macro_metadata",
        "node_provenance",
        "scope_arena",
        "alias_table",
        "shadow_table",
        "files",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();

    assert_eq!(
        code_graph_fields, expected,
        "CodeGraph::assert_no_tombstone_residue_for must iterate exactly the K-row fields \
         {expected:?}, got {code_graph_fields:?}. \
         When adding a K-row, extend this helper AND \
         RebuildGraph::assert_no_tombstone_residue AND the plan."
    );
    assert_eq!(
        rebuild_graph_fields, expected,
        "RebuildGraph::assert_no_tombstone_residue must iterate exactly the K-row fields \
         {expected:?}, got {rebuild_graph_fields:?}. \
         When adding a K-row, extend this helper AND \
         CodeGraph::assert_no_tombstone_residue_for AND the plan."
    );
    assert_eq!(
        code_graph_fields, rebuild_graph_fields,
        "Residue-helper K-row drift: CodeGraph has {code_graph_fields:?} but RebuildGraph \
         has {rebuild_graph_fields:?}. A future K-row addition must extend BOTH helpers; \
         this test exists to enforce that contract."
    );
}

/// Synthetic test: the visitor must spot a single `all_node_ids()` loop
/// inside a trivial function body. Keeps the visitor honest against
/// cosmetic source edits.
#[test]
fn residue_field_visitor_extracts_single_loop() {
    let src = "
        impl Foo {
            pub fn bar(&self) {
                for _nid in self.only_field.all_node_ids() {}
            }
        }
    ";
    let fields = parse_residue_helper_k_row_fields(src, "bar");
    assert_eq!(fields, BTreeSet::from(["only_field".to_string()]));
}

/// Synthetic test: the visitor ignores other method calls / other
/// loops — only `self.<field>.all_node_ids()` is counted.
#[test]
fn residue_field_visitor_ignores_unrelated_calls() {
    let src = "
        impl Foo {
            pub fn bar(&self) {
                for _nid in self.a.all_node_ids() {}
                for _x in self.b.iter() {} // ignored — not all_node_ids
                for _c in other_fn() {} // ignored — not a self.<field>
                for _nid in self.c.all_node_ids() {}
            }
        }
    ";
    let fields = parse_residue_helper_k_row_fields(src, "bar");
    assert_eq!(fields, BTreeSet::from(["a".to_string(), "c".to_string()]));
}

/// Recursively walk `dir`, pushing any `.rs` file whose contents
/// contain an `assert_impl_all!(...: NodeIdBearing)` invocation into
/// `found` (commented-out lines are ignored via
/// [`parse_assert_impl_all_types`]).
fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, found);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let Ok(src) = fs::read_to_string(&path) else {
            continue;
        };
        if !parse_assert_impl_all_types(&src).is_empty() {
            found.push(path);
        }
    }
}
