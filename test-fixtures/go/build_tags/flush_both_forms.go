//go:build linux
// +build darwin

package buildtags

// FlushBothForms exercises the precedence rule from 02_DESIGN §4.3.c
// rule 4 / 01_SPEC §6.3 AC-T3.8-3: when both //go:build and
// // +build are present and disagree, //go:build wins.
// Expected cfg_condition: "linux" (the //go:build directive's payload).
// The build constraints parser MUST also emit a warn-level log noting
// the disagreement (per AC-T3.8-3's "log a warning").
func FlushBothForms() {}
