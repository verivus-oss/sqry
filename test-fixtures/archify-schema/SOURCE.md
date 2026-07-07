# Vendored Archify JSON schemas

These two files are copied verbatim from the upstream Archify project and are
used only by sqry's schema-conformance tests (they are never compiled into any
shipped binary). sqry's Archify exporter emits JSON that must validate against
`architecture.schema.json` (which `$ref`s `common.schema.json`).

## Pin

- Upstream repo: https://github.com/tt-a1i/archify
- Path: `archify/schemas/architecture.schema.json`, `archify/schemas/common.schema.json`
- Pinned commit: `fa7fbce7fba812fbb8ac813408602412806fe40f` (branch `main`)
- Vendored on: 2026-07-06

## Why vendored (not fetched at test time)

The conformance tests must be hermetic: no network, no Node/ajv runtime. We
validate against the vendored copy with the Rust `jsonschema` crate
(draft 2020-12). Vendoring also makes any upstream schema change a visible,
deliberate diff rather than a silent behaviour shift.

## Refresh trigger and owner

Archify is pre-v3 ("JSON IR stabilization" is a roadmap item). `schema_version`
is `const: 1` today; upstream states a v1 file keeps rendering identically
across every 2.x release, with a bump to `2` only on a breaking change.

Refresh this pin when either is true:

- Archify tags a new release whose schema changes the `architecture` or shared
  `common` definitions (watch the upstream `archify/schemas/` directory).
- sqry wants to target a new `schema_version`.

Refresh procedure: re-download both files at the new commit, update the pin +
date above, run `cargo test -p sqry-core archify`, and reconcile any generated
sample that no longer validates. Owner/trigger: the sqry maintainer landing the
schema-version bump (tracked against issue verivus-oss/sqry#480 and its
follow-ups). Do not bump silently: a `schema_version: 2` is a coordinated,
reviewed change.
