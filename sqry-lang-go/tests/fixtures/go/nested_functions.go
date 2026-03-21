package main

func outer() {
	inner := func() {
		helper()
	}
	inner()
}

func helper() {
	// Helper function
}

func main() {
	outer()
}
