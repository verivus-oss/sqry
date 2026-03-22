// Test fixture: Qualified method calls
// Tests: Fully qualified calls, java.lang.*, java.util.*

package com.example.qualified;

public class QualifiedCalls {

    public static void main(String[] args) {
        // Fully qualified standard library calls
        String s1 = java.lang.String.valueOf(42);
        int len = java.lang.Math.max(10, 20);
        double sqrt = java.lang.Math.sqrt(16.0);

        // Collections framework
        java.util.List<String> list = new java.util.ArrayList<>();
        list.add("item");
        int size = list.size();

        java.util.Map<String, Integer> map = new java.util.HashMap<>();
        map.put("key", 100);
        Integer value = map.get("key");

        // StringBuilder
        java.lang.StringBuilder sb = new java.lang.StringBuilder();
        sb.append("Hello");
        sb.append(" World");
        String result = sb.toString();

        // System calls
        long time = java.lang.System.currentTimeMillis();
        java.lang.System.out.println("Time: " + time);

        if (s1 != null && value != null && result != null && size >= 0) {
            time = time + len + (long) sqrt;
        }
    }
}
