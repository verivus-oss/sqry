// T3.6 AC-T3.6-1 fixture: single `%w` wrap site.
// Sqry should emit exactly one Wraps{ErrorfVerb, chain_position: None}
// edge sourced at `wrap` (the enclosing function) targeting `inner`.
package errorwrapping

import "fmt"

func wrap(inner error) error {
	return fmt.Errorf("wrap: %w", inner)
}
