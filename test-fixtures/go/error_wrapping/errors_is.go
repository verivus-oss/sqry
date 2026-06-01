// T3.6 AC-T3.6-5 fixture: `errors.Is(err, sentinel)` call.
// Sqry should emit one Wraps{ErrorsIs, None} edge from the errors.Is
// call site to ErrNotFound. Reverse traversal: trace_path from
// ErrNotFound to check returns a non-empty path.
package errorwrapping

import "errors"

var ErrNotFound = errors.New("not found")

func check(err error) bool {
	return errors.Is(err, ErrNotFound)
}
