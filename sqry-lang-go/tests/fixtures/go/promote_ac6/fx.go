// Package fx is the AC-6 fixture per 01_SPEC.md §7 AC-6: pointer-
// required promotion does not over-promote. Inner declares Mutate
// with a pointer receiver. OuterV embeds Inner (value); OuterP
// embeds *Inner (pointer).
//
// Pass behaviour expected:
//   - fx.OuterV does NOT satisfy Mutator (only *fx.OuterV does).
//   - fx.OuterP DOES satisfy Mutator.
package fx

type Mutator interface {
	Mutate()
}

type Inner struct{}

func (*Inner) Mutate() {}

type OuterV struct {
	Inner
}

type OuterP struct {
	*Inner
}
