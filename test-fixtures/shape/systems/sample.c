/* Hand-written control-flow sample for the C body-shape descriptor coverage
 * test. Exercises branch, loop, switch, return, break/continue, call, and
 * assignment so the canonical CfBucket histogram is non-empty. */

extern int compute(int value);
extern void emit(int value);

int classify(int n, const char *label, ...) {
    int total = 0;
    if (n > 0) {
        total = compute(n);
    } else {
        total = 0;
    }

    for (int i = 0; i < n; i++) {
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

    return total;
}
