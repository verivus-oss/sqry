// Test cases for Swift async detection
// FT-B.1 (Swift Async Fix)
//
// This file contains both TRUE POSITIVES (real async functions)
// and FALSE NEGATIVES (should NOT be detected as async)

// ========================================
// TRUE POSITIVES - Should detect async
// ========================================

func realAsync() async -> String {
    return "This is async"
}

func asyncThrows() async throws -> Data {
    return Data()
}

func throwsAsync() throws async -> Int {
    return 42
}

// ========================================
// FALSE NEGATIVES - Should NOT detect async
// ========================================

// 1. Comment with "async" keyword
func commentAsync() -> Void {
    // This function is not async despite the comment
    print("sync")
}

// 2. String literal with "async" keyword
func stringAsync() -> Void {
    let message = "call async function"
    print(message)
}

// 3. Identifier containing "async" keyword
func identifierAsync() -> Void {
    let asyncVar = 123
    let myAsyncValue = 456
    print(asyncVar + myAsyncValue)
}

// 4. Multiple false negatives in one function
func multipleAsyncMentions() -> Void {
    // async is mentioned here
    let str = "async operation"
    var asyncCounter = 0
    // Another async comment
    print("\(str) \(asyncCounter)")
}
