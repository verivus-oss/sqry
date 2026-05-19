// Package fx is the AC-8 fixture per 01_SPEC.md §7 AC-8: empty
// interface filter. The universal interface `any` (alias for
// `interface{}`) must be excluded from the Implements graph per
// 01_SPEC.md §5.7.
package fx

type X struct{}

func (X) M() {}

// Two named interfaces — one populated (`HasM`) and one empty
// (`Empty`). The pass must emit Implements(X → HasM) but must NOT emit
// Implements(X → Empty) — Empty is uninteresting per §5.7.
type HasM interface {
	M()
}

type Empty interface{}

// Force `Empty` to be reachable as a node in the graph so the
// assertion isn't trivially satisfied by absence.
var _ Empty = X{}
