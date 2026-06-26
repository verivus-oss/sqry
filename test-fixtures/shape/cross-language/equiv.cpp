// AC-6 cross-language equivalence anchor (C++ side). Hand-written, MIT-clean.
//
// Each function here is the structural twin of the same-named function in
// equiv.py. See equiv.py for the full rationale: `branchy` matches its Python
// twin on the full canonical histogram; `classify` matches on every control-flow
// bucket and differs only in the Assign bucket, because the range-for below
// declares `int item` (an Assign) where Python's `for ... in` declares nothing.

#include <vector>

int helper(int n);

int branchy(int x) {
    if (x > 0) {
        helper(x);
        return 1;
    }
    if (x < 0) {
        helper(x);
        return 2;
    }
    return 0;
}

int classify(int x, const std::vector<int> &items) {
    if (x > 0) {
        helper(x);
        return 1;
    }
    for (int item : items) {
        helper(item);
    }
    return 0;
}
