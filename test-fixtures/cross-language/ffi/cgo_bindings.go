package main

// #include <math_utils.h>
import "C"
import "fmt"

func main() {
	result := C.calculate_sum(3, 4)
	fmt.Println("Sum:", result)
}
