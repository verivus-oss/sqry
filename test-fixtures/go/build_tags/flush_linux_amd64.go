//go:build linux && amd64

package buildtags

// FlushLinuxAmd64 exercises a single //go:build header with `&&` of two
// platform tokens. Per 02_DESIGN §10.3 + 01_SPEC §6.3 AC-T3.8-1 the
// canonical header form is `"linux && amd64"`. Because this fixture's
// basename ends with the recognised `_linux_amd64` GOOS/GOARCH
// double-suffix, `parse_filename_suffix` ALSO returns
// `All([linux, amd64])` and the conjoin step (see
// `sqry-lang-go/src/relations/build_constraints.rs::conjoin` — flatten
// without dedup) produces a duplicated stored cfg_condition of
// `"linux && amd64 && linux && amd64"`. The AC-T3.8-1 canonical-form
// invariant is asserted at the byte-level in the unit test
// `ac_t3_8_1_gobuild_line_canonical_form` (sqry-lang-go/tests/
// cfg_stamping.rs) which uses a non-suffix filename to isolate the
// header parser. This fixture's job is integration-level: the file
// must surface SOME `linux`+`amd64`-bearing cfg_condition end-to-end
// through `sqry index`. Codex iter-5 surfaced the importance of
// keeping that distinction explicit, hence the long comment.
func FlushLinuxAmd64() {}
