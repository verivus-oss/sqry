package com.example.localvars;

class UseBeforeDeclOuter {
    void test() {
        int y = 1;
        {
            int z = y; // resolves to outer y
            int y = 5; // new y shadows outer, but z already resolved to outer
            System.out.println(y);
        }
    }
}
