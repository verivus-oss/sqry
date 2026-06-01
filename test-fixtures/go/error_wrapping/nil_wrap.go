// T3.6 spec §5.1.e fixture: nil-target corner.
// `fmt.Errorf("ctx: %w", nil)` MUST emit zero Wraps edges (no resolved
// target to wrap). The Calls edge to fmt.Errorf still exists.
package errorwrapping

import "fmt"

func wrapNil() error {
	return fmt.Errorf("ctx: %w", nil)
}
