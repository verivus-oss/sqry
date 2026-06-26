// Hand-written C++ sample exercising the real control-flow kinds the
// body-shape descriptor buckets. MIT-clean, no vendored sources.

#include <vector>
#include <string>

int classify(const std::vector<int> &values, int threshold = 0) {
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
    } catch (const std::exception &err) {
        throw std::runtime_error("classify failed");
    }
    switch (total) {
        case 0:
            return 0;
        default:
            return total;
    }
}

auto adder(int base) {
    return [base](int n) { return base + n; };
}
