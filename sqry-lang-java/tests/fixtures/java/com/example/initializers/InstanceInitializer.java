package com.example.initializers;

class InstanceInitializer {
    int value;

    {
        int x = 1;
        value = x;
        System.out.println(x);
    }
}
