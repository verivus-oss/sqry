package com.example.fields;

class ThisFieldPrecedence {
    int x = 10;

    void test() {
        int x = 1;
        System.out.println(x);       // resolves to local x
        System.out.println(this.x);  // resolves to field, skip local
    }
}
