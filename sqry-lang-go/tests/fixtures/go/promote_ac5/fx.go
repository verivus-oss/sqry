// Package fx is the AC-5 fixture per 01_SPEC.md §7 AC-5: promoted
// method is queryable from the outer type. Outer embeds Inner, so
// Greeting (declared on Inner) is reachable as Outer.Greeting via
// promotion.
package fx

type Inner struct{}

func (Inner) Greeting() {}

type Outer struct {
	Inner
}

func use() {
	var o Outer
	o.Greeting()
}
