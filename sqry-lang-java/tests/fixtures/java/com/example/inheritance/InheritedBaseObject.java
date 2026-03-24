package com.example.inheritance;

class InheritedBaseObject {
    void test() {
        int x = 1;
        Object obj = new Object() {
            void use() {
                System.out.println(x);
            }
        };
        System.out.println(obj.toString());
    }
}
