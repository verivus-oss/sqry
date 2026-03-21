package com.example.classes;

class LocalEnumNoCapture {
    void test() {
        int x = 1;
        enum E {
            A;
            int use() {
                // x should NOT be captured (enum is static context)
                return 0;
            }
        }
    }
}
