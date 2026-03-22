package com.example.trycatch;

class MultiCatchUnion {
    void test() {
        try {
            System.out.println("try");
        } catch (IllegalArgumentException | NullPointerException e) {
            System.out.println(e);
        }
    }
}
