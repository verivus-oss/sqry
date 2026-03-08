package com.example.inheritance;

class InheritedBase {
    int x = 10;
}

class InheritedFieldPrecedence {
    void test() {
        int x = 1;
        Object obj = new InheritedBase() {
            void use() {
                System.out.println(x); // inherited member wins
            }
        };
    }
}
