package com.example.classes;

class LocalClassCapture {
    void test() {
        int x = 1;
        class Local {
            void use() {
                System.out.println(x); // captures outer x
            }
        }
    }
}
