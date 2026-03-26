package com.example.classes;

class LocalRecordNoCapture {
    void test() {
        int x = 1;
        record R() {
            void use() {
                // x should NOT be captured (record is static context)
                System.out.println("no capture");
            }
        }
    }
}
