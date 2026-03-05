package com.example.patterns;

class ForPatternScopes {
    void test(Object obj) {
        for (int i = 0; obj instanceof String s; i++) {
            System.out.println(s);
            System.out.println(i);
            break;
        }
    }
}
