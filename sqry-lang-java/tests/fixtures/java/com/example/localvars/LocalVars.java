package com.example.localvars;

class LocalVars {
    void simple() {
        int x = 5;
        System.out.println(x);
    }

    void paramRef(int y) {
        System.out.println(y);
    }

    void multipleVars() {
        int a = 1;
        int b = 2;
        int c = a + b;
        System.out.println(c);
    }
}
