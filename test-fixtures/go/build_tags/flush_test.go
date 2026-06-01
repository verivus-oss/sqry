package buildtags

import "testing"

// TestFlushNoOp pins the AC-T3.8-6 invariant: filename ends in
// `_test.go`, so the `_test` suffix is stripped before suffix parsing
// and yields cfg_condition = None. The Test* function signature here
// keeps `go test ./...` happy in case anyone runs the fixtures with
// the Go toolchain.
func TestFlushNoOp(t *testing.T) {
	_ = t
}
