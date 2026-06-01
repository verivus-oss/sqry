//go:build go1.26

// T3.6 AC-T3.6-6b fixture: `errors.AsType[T](err)` (Go 1.26+).
// Sqry should emit one Wraps{ErrorsAsType, None} edge from the
// errors.AsType call site to fs.PathError. Gated behind `go1.26` so
// older Go toolchains skip rather than fail.
package errorwrapping

import (
	"errors"
	"io/fs"
)

func extractAsType(err error) (*fs.PathError, bool) {
	return errors.AsType[*fs.PathError](err)
}
