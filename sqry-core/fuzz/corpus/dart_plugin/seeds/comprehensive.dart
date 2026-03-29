import 'dart:async';

class Counter {
  int _count = 0;

  int get count => _count;

  Future<void> incrementAsync() async {
    await Future.delayed(Duration(milliseconds: 100));
    _count++;
  }
}

void main() {
  final counter = Counter();
  print(counter.count);
}
