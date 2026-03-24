package com.example.patterns;

class PatternDeclFilter {
    void instanceofPattern(Object obj) {
        if (obj instanceof String s) {
            System.out.println(s);  // usage of s
        }
    }

    int switchPattern(Object obj) {
        return switch (obj) {
            case Integer i -> i + 1;    // usage of i
            case String s -> s.length(); // usage of s
            default -> 0;
        };
    }
}
