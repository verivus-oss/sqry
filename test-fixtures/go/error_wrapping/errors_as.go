// T3.6 AC-T3.6-6 fixture: `errors.As(err, &target)` call.
// Sqry should emit one Wraps{ErrorsAs, None} edge from the errors.As
// call site to fs.PathError (the pointer-element type, not *fs.PathError).
package errorwrapping

import (
	"errors"
	"io/fs"
)

func extract(err error) (*fs.PathError, bool) {
	var target *fs.PathError
	if errors.As(err, &target) {
		return target, true
	}
	return nil, false
}
