class MalformedExample {
    func incompleteMethod(
        // Missing closing parenthesis and body

    func anotherMethod() {
        // This should still be extracted
    }
}

// Unclosed class
class Incomplete {
    func test() {
        anotherMethod()
