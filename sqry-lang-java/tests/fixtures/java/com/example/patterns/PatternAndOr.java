package com.example.patterns;

class PatternAndOr {
    void andInsideOr(Object obj) {
        if ((obj instanceof String s && s.length() > 0) || obj == null) {
            // s NOT in scope for then block (|| breaks global check)
            // but s IS in scope for s.length() (local && check)
        }
    }

    void outerNegation(Object obj) {
        if (!(obj instanceof String s && s.length() > 0)) {
            // outer ! doesn't affect && local scope
            // s IS in scope for s.length()
        }
    }
}
