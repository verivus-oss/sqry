package com.example.fields;

class EnclosingFieldPrecedence {
    int x = 10;

    void test() {
        int x = 1;
        Runnable r = new Runnable() {
            public void run() {
                System.out.println(x); // captured local wins (no member in anon class)
            }
        };
    }
}
