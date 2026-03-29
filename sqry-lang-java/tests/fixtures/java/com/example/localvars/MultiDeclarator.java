package com.example.localvars;

class MultiDeclarator {
    void test() {
        int a = 1, b = a, c = b;
        System.out.println(c);
    }

    void selfBinding() {
        int x = 1;
        {
            int y = y; // invalid but should not create self-edge
        }
    }
}
