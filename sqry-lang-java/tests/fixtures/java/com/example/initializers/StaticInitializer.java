package com.example.initializers;

class StaticInitializer {
    static int value;

    static {
        int x = 1;
        value = x;
        System.out.println(x);
    }
}
