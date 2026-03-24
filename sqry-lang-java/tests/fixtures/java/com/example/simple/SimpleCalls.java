// Test fixture: Basic method calls
// Tests: main→greet, processData→fetch→transform

package com.example.simple;

public class SimpleCalls {

    public static String greet(String name) {
        return String.format("Hello, %s!", name);
    }

    public static String fetch(int id) {
        if (id > 0) {
            return String.format("Data-%d", id);
        }
        return "";
    }

    public static String transform(String data) {
        return data.toUpperCase();
    }

    public static String processData(int id) {
        String raw = fetch(id);
        if (!raw.isEmpty()) {
            return transform(raw);
        }
        return "";
    }

    public static void main(String[] args) {
        String message = greet("World");
        System.out.println(message);

        String result = processData(42);
        if (!result.isEmpty()) {
            System.out.println("Processed: " + result);
        }
    }
}
