package localvars

// For-range loop variables.
func forRange() {
	items := []int{1, 2, 3}
	for k, v := range items {
		_ = k
		_ = v
	}
}

// Multiple references to same variable.
func multiRef() {
	x := 1
	y := x + x
	z := x + y
	_ = z
}
