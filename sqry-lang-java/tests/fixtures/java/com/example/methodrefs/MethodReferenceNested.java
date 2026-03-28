package com.example.methodrefs;

class MethodReferenceNested {
    void test() {
        String obj = "hello";
        Runnable r = () -> {
            Runnable inner = obj::hashCode; // obj resolves through lambda capture
        };
    }
}
