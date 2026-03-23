package com.example.methodrefs;

class IdentifierFilterCases {
    void labels() {
        outer:
        for (int i = 0; i < 10; i++) {
            break outer; // outer is label, no reference edge
        }
    }

    void classLiteral() {
        Class<?> c = String.class; // String is type, no var reference
    }

    void staticQualified() {
        int max = Integer.MAX_VALUE; // Integer is type, no var reference
    }
}
