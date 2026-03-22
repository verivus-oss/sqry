package com.example.fields;

class SuperBase {
    int x = 10;
}

class SuperFieldPrecedence extends SuperBase {
    void test() {
        int x = 1;
        System.out.println(x);       // resolves to local x
        System.out.println(super.x); // resolves to field, skip local
    }
}
