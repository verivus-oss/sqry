package localvars

// Nested scope shadowing — inner x shadows outer x.
func shadowedVar() {
	x := 10
	_ = x
	{
		x := 20
		_ = x
	}
	_ = x
}

// If-statement init variable scoping.
func ifInitVar() {
	x := 5
	if y := x + 1; y > 0 {
		_ = y
	}
	_ = x
}

// For loop variable scoping.
func forLoopVar() {
	for i := 0; i < 10; i++ {
		_ = i
	}
}
