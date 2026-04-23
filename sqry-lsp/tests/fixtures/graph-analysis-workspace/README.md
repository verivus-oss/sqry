# Graph Analysis Workspace Fixture

This fixture backs the PN2 LSP graph-analysis handler tests.

Expected call-cycle members:
- `["cycle_ab_start", "cycle_ba_partner"]`
- `["mod_cycle_a_entry", "mod_cycle_b_entry"]`
- `["reach_cycle_left", "reach_cycle_right"]`
- `["bulk_cycle_alpha", "bulk_cycle_beta"]`
- `["utf16_cycle_end", "utf16_cycle_start"]`
- `["recurse_self_loop"]` only when self loops are enabled

Expected import/module-cycle members:
- module-level cycle between `mod_cycle_a` and `mod_cycle_b`

Expected reachability exclusions from unused results:
- `imported_only_symbol` stays reachable through `Imports`
- `REFERENCED_ONLY_CONST` stays reachable through `References`
- `UsedViaTypeOf` stays reachable through `TypeOf`
- `main` is excluded as an entry point

Bulk unused symbols:
- `unused_bulk.rs` defines `_unused_001` through `_unused_101`
- `unused_bulk.rs` also defines `OrphanStruct`

UTF-16-sensitive symbols:
- `utf16_unused_marker`
- `utf16_cycle_start`
