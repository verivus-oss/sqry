package main

import "fmt"

func helper() {
	fmt.Println("Helper called")
}

func main() {
	helper()
	fmt.Println("Main function")
}
