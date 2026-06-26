// Hand-written Java sample exercising real control-flow kinds the body-shape
// descriptor buckets. MIT-clean, no vendored sources.

import java.util.List;

class Sample {
    int classify(List<Integer> values, int threshold) {
        int total = 0;
        for (int value : values) {
            if (value > threshold) {
                total += value;
            } else if (value < 0) {
                continue;
            } else {
                break;
            }
        }
        int index = 0;
        while (index < total) {
            index++;
        }
        try {
            helper(total);
        } catch (RuntimeException err) {
            throw new IllegalStateException("classify failed");
        }
        switch (total) {
            case 0:
                return 0;
            default:
                return total;
        }
    }

    Runnable adder(int base) {
        return () -> System.out.println(base);
    }
}
