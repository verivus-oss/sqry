package com.example.trycatch;

import java.io.Closeable;
import java.io.IOException;

class ResourceInitializerChain {
    Closeable wrap(Closeable inner) { return inner; }

    void test() throws IOException {
        try (Closeable a = null; Closeable b = wrap(a)) {
            System.out.println(a);
            System.out.println(b);
        }
    }
}
