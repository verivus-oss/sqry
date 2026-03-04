package com.example.patterns;

class PatternScopes {
    void ifPattern(Object obj) {
        if (obj instanceof String s) {
            System.out.println(s);
        }
    }

    void whilePattern(Object obj) {
        while (obj instanceof String s) {
            System.out.println(s);
            break;
        }
    }

    void forPattern(Object obj) {
        for (; obj instanceof String s; ) {
            System.out.println(s);
            break;
        }
    }

    int ternaryPattern(Object obj) {
        return obj instanceof String s ? s.length() : 0;
    }

    void andPattern(Object obj) {
        if (obj instanceof String s && s.length() > 0) {
            System.out.println(s);
        }
    }
}
