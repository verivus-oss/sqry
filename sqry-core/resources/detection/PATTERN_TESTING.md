# Pattern Validation & Maintenance

**Last Updated**: 2025-10-11  
**Owners**: Intelligent Code Detection Working Group

---

## Purpose

Document the workflow for maintaining the YAML-based language pattern library used by the Intelligent Code Detection system. This ensures consistency, auditability, and high accuracy as heuristics evolve.

---

## Review Cadence

- **Monthly**: Quick scan for pending issues or community reports.
- **Quarterly**: Deep review of top 10 languages; update weights based on corpus metrics.
- **As-needed**: Immediate follow-up when accuracy regressions are detected in CI.

---

## Update Workflow

1. **Open Ticket**: Describe motivation, sample files, and expected impact.
2. **Create Branch**: Update relevant `languages/<lang>.yaml` files.
3. **Run Validation Script**:
   ```bash
   cargo test -p sqry-core pattern_database_loads_yaml
   cargo test -p sqry-core detection_content_tests -- --ignored pattern_regressions
   ```
4. **Corpus Evaluation**: Execute accuracy benchmark against labeled corpus:
   ```bash
   cargo test -p sqry-core detection_accuracy_corpus -- --ignored
   ```
   Record precision/recall deltas in the PR description.
5. **Peer Review**: At least one maintainer familiar with the language must approve.
6. **Update Version**: Bump `version` field in the YAML file (e.g., `1.0.1`) to invalidate caches.
7. **Document Change**: Append summary to this file under “Change Log”.

---

## Automated Checks

- `pattern_database_loads_yaml`: Ensures schema validity and non-negative weights.
- `pattern_regressions`: Runs corpus-based accuracy regression tests (ignored by default; enable with `-- --ignored`).
- `cargo fmt` & `cargo clippy`: Enforce coding style in helper scripts if updated.

---

## Change Log

| Date | Language(s) | Version | Summary | PR |
|------|-------------|---------|---------|----|
| - | - | - | Initial guide placeholder | - |

---

## Future Enhancements

- Automate corpus evaluation via CI workflow.
- Track historical precision/recall metrics per change.
- Allow pattern overrides to be supplied via `.sqry/detection.yaml` for local experimentation.

