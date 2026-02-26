package com.example.inheritance;

class ResolvedBase {
    int value = 10;
}

class InheritedResolved {
    void test() {
        int value = 1;
        Object obj = new ResolvedBase() {
            void use() {
                System.out.println(value); // inherited member wins
            }
        };
    }
}
