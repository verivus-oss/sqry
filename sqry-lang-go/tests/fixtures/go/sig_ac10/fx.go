// Package fx is the AC-10 fixture per 01_SPEC.md §7 AC-10: function-
// signature implements with a named function type that has no
// methods. T1.3 emits Implements(double → Op) on the conversion
// Op(double).
package fx

type Op func(int) int

func double(x int) int { return x * 2 }

var _ = Op(double)
