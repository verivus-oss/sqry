package com.example.patterns;

record Point(int x, int y) {}

class RecordPatternInstanceof {
    void test(Object obj) {
        if (obj instanceof Point(int x, int y)) {
            System.out.println(x);
            System.out.println(y);
        }
    }
}
