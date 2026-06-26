# AC-6 cross-language equivalence anchor (Python side). Hand-written, MIT-clean.
#
# Each function here has a byte-for-byte structural twin in equiv.cpp. The point
# is NOT that the source text matches (it cannot, two languages) but that the
# identifier-blind ShapeDescriptor's canonical control-flow histogram matches,
# proving the one CfBucket schema is genuinely cross-language.
#
# `branchy` uses only branches/calls/returns, so EVERY canonical bucket matches
# its C++ twin exactly (a full-histogram identity). `classify` adds a loop; the
# C++ range-for declares a loop variable (an Assign bucket count) that Python's
# `for ... in` does not, so the two agree on every CONTROL-FLOW bucket and differ
# only in that one language-idiom bucket. That is exactly the "comparable, not
# identical" claim AC-6 makes.


def branchy(x):
    if x > 0:
        helper(x)
        return 1
    if x < 0:
        helper(x)
        return 2
    return 0


def classify(x, items):
    if x > 0:
        helper(x)
        return 1
    for item in items:
        helper(item)
    return 0
