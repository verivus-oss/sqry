package com.example.classes;

class LocalInterfaceNoCapture {
    void test() {
        int x = 1;
        interface I {
            // x should NOT be captured (interface is static context)
            int C = 0;
        }
    }
}
