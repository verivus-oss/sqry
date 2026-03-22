package com.example.trycatch;

import java.io.Closeable;
import java.io.IOException;

class TryResourceVisibility {
    void test() throws IOException {
        try (Closeable r = null) {
            System.out.println(r);
        } catch (Exception e) {
            // r is NOT visible here
            System.out.println(e);
        } finally {
            // r is NOT visible here
            System.out.println("done");
        }
    }
}
