package com.example.inheritance;

interface KnownInterface {
    int KNOWN = 1;
}

class InheritedPartialResolution {
    void test() {
        int KNOWN = 2;
        Object obj = new Object() {
            // one resolved (KnownInterface), one would be unresolved
            void use() {
                System.out.println(KNOWN);
            }
        };
    }
}
