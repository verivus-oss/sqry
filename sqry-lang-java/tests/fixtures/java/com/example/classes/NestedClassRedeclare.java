package com.example.classes;

class NestedClassRedeclare {
    void test() {
        int x = 1;
        class Local {
            void use() {
                int x = 2; // allowed - class boundary
                System.out.println(x);
            }
        }
    }
}
