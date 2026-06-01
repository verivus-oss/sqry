//go:build linux

package buildtags

// FlushFilenameArm64 exercises AC-T3.8-5's filename-suffix + header
// conjunction. The filename `flush_filename_arm64.go` ends in a
// known GOARCH token (`arm64`), so `parse_filename_suffix` returns
// `Flag("arm64")`. The `//go:build linux` header contributes
// `Flag("linux")`. Conjoin order is filename-first per 01_SPEC §6.3
// AC-T3.8-5, so the stored cfg_condition is `"arm64 && linux"` —
// distinct from the sibling `flush_linux_amd64.go` (single-header
// `linux && amd64`) and `flush_filename_linux.go` (filename-only
// `linux`).
//
// Closes codex iter-5 finding 3: the previous fixture at
// `flush_filename_and_header.go` had no GOOS/GOARCH suffix in its
// basename, so `parse_filename_suffix` returned `None` and the file
// was effectively header-only — defeating the AC-T3.8-5 invariant.
func FlushFilenameArm64() {}
