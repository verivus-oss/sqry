// Test cases for Dart async detection
// PoC: FT-B.0 (Dart Async PoC)
//
// This file contains both TRUE POSITIVES (real async functions)
// and FALSE NEGATIVES (should NOT be detected as async)

// ========================================
// TRUE POSITIVES - Should detect async
// ========================================

Future<void> realAsync() async {
  print("This is async");
}

Future<String> asyncArrow() async => "result";

Future<int> asyncStar() async* {
  yield 1;
  yield 2;
}

// ========================================
// FALSE NEGATIVES - Should NOT detect async
// ========================================

// 1. Comment with "async" keyword
void commentAsync() {
  // This function is not async despite the comment
  print("sync");
}

// 2. String literal with "async" keyword
void stringAsync() {
  String message = "call async function";
  print(message);
}

// 3. Identifier containing "async" keyword
void identifierAsync() {
  var asyncVar = 123;
  var myAsyncValue = 456;
  print(asyncVar + myAsyncValue);
}

// 4. Multiple false negatives in one function
void multipleAsyncMentions() {
  // async is mentioned here
  String str = "async operation";
  var asyncCounter = 0;
  // Another async comment
  print("$str $asyncCounter");
}
