// T3.6 AC-T3.6-7 fixture: `errors.Join(a, b, c)` with exactly 3 args.
// Sqry should emit exactly 3 Wraps{ErrorsJoin, Some(0|1|2)} edges from
// the errors.Join call site, one per argument in order.
package errorwrapping

import "errors"

func bundle(a, b, c error) error {
	return errors.Join(a, b, c)
}
