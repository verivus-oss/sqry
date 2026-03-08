package localvars

// Basic local variable declaration and usage.
func basicVars() {
	x := 10
	y := x + 1
	_ = y
}

// Var declaration with type.
func varDecl() {
	var count int
	count = 42
	_ = count
}

// Multiple short var declarations.
func multiShortVar() {
	a, b := 1, 2
	c := a + b
	_ = c
}

// Parameter references.
func paramRef(name string, age int) {
	result := name
	_ = result
	_ = age
}
