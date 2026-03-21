package com.example.classes;

class LocalRecordNestedCapture {
    void test() {
        int x = 1;
        record R() {
            void use() {
                Runnable r = new Runnable() {
                    public void run() {
                        // x should NOT be captured even through anon class
                        // because record boundary blocks capture
                        System.out.println("no capture");
                    }
                };
            }
        }
    }
}
