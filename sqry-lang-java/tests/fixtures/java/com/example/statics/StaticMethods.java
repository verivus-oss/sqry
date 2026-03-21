// Test fixture: Static method calls
// Tests: Static method invocations, qualified calls

package com.example.statics;

public class StaticMethods {

    public static class MathUtils {
        public static int add(int a, int b) {
            return a + b;
        }

        public static int multiply(int a, int b) {
            return a * b;
        }

        public static int square(int x) {
            return multiply(x, x);
        }
    }

    public static class StringUtils {
        public static String reverse(String s) {
            return new StringBuilder(s).reverse().toString();
        }

        public static boolean isEmpty(String s) {
            return s == null || s.length() == 0;
        }
    }

    public static void main(String[] args) {
        int sum = MathUtils.add(5, 3);
        int product = MathUtils.multiply(4, 7);
        int squared = MathUtils.square(5);

        String reversed = StringUtils.reverse("hello");
        boolean empty = StringUtils.isEmpty("");

        if (sum + product + squared > 0 && reversed != null && !empty) {
            MathUtils.add(sum, squared);
        }
    }
}
