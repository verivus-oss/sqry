package com.example.inheritance;

class InheritedExplicitUnresolved {
    void test() {
        int x = 1;
        // UnknownBase is not defined in this file - unresolved
        Object obj = new UnknownBase() {
            void use() {
                System.out.println(x); // ambiguous - skip local capture
            }
        };
    }
}
