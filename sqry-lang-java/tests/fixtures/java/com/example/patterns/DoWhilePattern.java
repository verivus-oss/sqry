package com.example.patterns;

class DoWhilePattern {
    void test(Object obj) {
        do {
            // s is NOT in scope here
            System.out.println("loop");
        } while (obj instanceof String s);
    }
}
