package com.example.inheritance;

interface HasConstant {
    int VALUE = 42;
}

class InterfaceFieldPrecedence {
    void test() {
        int VALUE = 1;
        Object obj = new HasConstant() {
            void use() {
                System.out.println(VALUE); // interface constant wins
            }
        };
    }
}
