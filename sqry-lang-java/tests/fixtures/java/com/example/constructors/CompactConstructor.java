package com.example.constructors;

record CompactConstructor(int x, int y) {
    CompactConstructor {
        if (x < 0) throw new IllegalArgumentException("x negative");
        System.out.println(y);
    }
}
