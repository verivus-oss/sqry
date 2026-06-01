// T3.6 AC-T3.6-2 fixture: multi-`%w` wrap site.
// Sqry should emit exactly two Wraps{ErrorfVerb} edges with
// chain_position Some(0) and Some(1) in argument order.
package errorwrapping

import "fmt"

func wrapMulti(first, second error) error {
	return fmt.Errorf("first=%w second=%w", first, second)
}
