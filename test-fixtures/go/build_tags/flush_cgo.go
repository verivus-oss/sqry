package buildtags

// #include <stdlib.h>
import "C"

// FlushCgo exercises the `import "C"` pseudo-import. Per 01_SPEC §6.3
// AC-T3.8-7, the presence of `import "C"` raises an implicit `cgo`
// constraint. With no //go:build header, no // +build line, and no
// platform-bearing filename suffix, the expected cfg_condition is
// "cgo".
func FlushCgo() {
	_ = C.RAND_MAX
}
