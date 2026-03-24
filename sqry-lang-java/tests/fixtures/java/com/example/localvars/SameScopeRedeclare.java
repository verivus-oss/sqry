package com.example.localvars;

class SameScopeRedeclare {
    void test() {
        int x = 1;
        System.out.println(x);
        int x = 2; // invalid Java - duplicate in same scope
        System.out.println(x);
    }
}
