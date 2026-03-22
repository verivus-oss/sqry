package com.example.inheritance;

interface BaseA {
    int shared = 1;
}

interface BaseB {
    int shared = 2;
}

class InheritedResolvedConflict {
    void test() {
        int shared = 3;
        Object obj = new Object() {
            // Both BaseA and BaseB define "shared" - ambiguous
            void use() {
                System.out.println(shared);
            }
        };
    }
}
