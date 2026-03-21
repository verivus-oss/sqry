package com.example.trycatch;

import java.io.Closeable;
import java.io.IOException;

class ResourceDeclInitializer {
    Closeable create(int x) { return null; }

    void test(int x) throws IOException {
        try (Closeable c = create(x)) {
            System.out.println(c);
        }
    }
}
