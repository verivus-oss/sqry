package com.example.methodrefs;

import java.util.function.Function;

class MethodReferenceCases {
    void typeMethodRef() {
        Function<String, Integer> f = String::length; // String is type, no ref
    }

    void exprMethodRef() {
        String s = "hello";
        Runnable r = s::hashCode; // s IS a variable reference
    }
}
