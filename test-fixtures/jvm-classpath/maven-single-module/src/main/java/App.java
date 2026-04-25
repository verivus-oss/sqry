package com.example;

import com.google.common.collect.ImmutableList;

public class App {
    public static void main(String[] args) {
        ImmutableList<String> items = ImmutableList.of("hello", "world");
        System.out.println(items);
    }
}
