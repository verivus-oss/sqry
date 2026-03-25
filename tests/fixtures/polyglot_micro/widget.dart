// Minimal Dart file for polyglot test fixture
// This file provides simple classes to verify Dart node extraction

class HelloWidget {
  final String message;

  HelloWidget(this.message);

  String greet() {
    return 'Hello from Dart: $message';
  }
}

class Counter {
  int value = 0;

  void increment() {
    value++;
  }

  void decrement() {
    value--;
  }

  int getValue() {
    return value;
  }
}
