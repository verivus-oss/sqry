// +build linux,amd64

package buildtags

// FlushPlusBuild exercises the legacy `// +build` header (no //go:build
// counterpart present). Expected cfg_condition (per 02_DESIGN §10.3 +
// 01_SPEC §6.3 AC-T3.8-2): "linux && amd64".
//
// The `,` separator within a single +build line means AND; whitespace
// between tokens on the same line would mean OR.
func FlushPlusBuild() {}
