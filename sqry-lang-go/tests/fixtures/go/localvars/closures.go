package localvars

// Closure capturing outer variable.
func closureCapture() {
	x := 10
	fn := func() int {
		return x
	}
	_ = fn
}

// Method with receiver reference.
type MyStruct struct {
	Value int
}

func (s *MyStruct) method() {
	v := s.Value
	_ = v
}

// Switch-case variable scoping.
func switchVar() {
	x := 3
	switch x {
	case 1:
		y := 10
		_ = y
	case 2:
		y := 20
		_ = y
	}
}
