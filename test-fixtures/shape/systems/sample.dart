// Hand-written control-flow sample for the Dart body-shape descriptor coverage
// test. Exercises branch, loop, switch, try/catch, throw, return, call, and
// assignment so the canonical CfBucket histogram is non-empty.

int compute(int value) => value * 2;

void emit(int value) {
  print(value);
}

int classify(int n, String label) {
  var total = 0;
  if (n > 0) {
    total = compute(n);
  } else {
    total = 0;
  }

  for (var i = 0; i < n; i++) {
    if (i == 3) {
      continue;
    }
    total += i;
  }

  while (total < 100) {
    total += 1;
    if (total == 50) {
      break;
    }
  }

  switch (n) {
    case 0:
      emit(total);
      break;
    default:
      emit(0);
      break;
  }

  try {
    emit(total);
  } catch (e) {
    throw StateError('failed');
  }

  return total;
}
