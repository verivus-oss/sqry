package com.example.constructors;

class VarargsParams {
    void test(String... args) {
        System.out.println(args.length);
        for (String s : args) {
            System.out.println(s);
        }
    }
}
