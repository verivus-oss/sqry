package com.example.trycatch;

import java.io.Closeable;
import java.io.IOException;

class TryWithResources {
    void resourceReuse(Closeable r) throws IOException {
        try (r) {
            System.out.println("using resource");
        }
    }

    void resourceDecl() throws IOException {
        try (Closeable c = null) {
            System.out.println(c);
        }
    }
}
