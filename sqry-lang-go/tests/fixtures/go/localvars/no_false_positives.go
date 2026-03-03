package localvars

import "fmt"

// Package-level variables should NOT be tracked.
var GlobalVar = 42

// Type names, function calls, imports, field access should NOT generate
// local variable References edges.
func noFalsePositives() {
	// Type name in conversion — should NOT be a local ref
	x := int(3.14)
	_ = x

	// Function call — should NOT be a local ref for "fmt" or "Println"
	fmt.Println("hello")

	// Field access after dot — should NOT be a local ref
	type Foo struct{ Bar int }
	f := Foo{Bar: 1}
	_ = f.Bar
}

// Labels should NOT be tracked.
func withLabel() {
outer:
	for i := 0; i < 10; i++ {
		if i == 5 {
			break outer
		}
		_ = i
	}
}
