// Go visibility test fixture
package main

// PublicFunction is exported (public)
func PublicFunction() string {
    return "public"
}

// privateFunction is not exported (private)
func privateFunction() string {
    return "private"
}
