// Test fixture: Lambda expressions
// Tests: Lambda syntax, functional interfaces

package com.example.lambdas;

import java.util.function.Function;
import java.util.function.Predicate;

public class Lambdas {

    interface Operation {
        int apply(int a, int b);
    }

    public static int compute(int x, int y, Operation op) {
        return op.apply(x, y);
    }

    public static boolean test(int value, Predicate<Integer> predicate) {
        return predicate.test(value);
    }

    public static void main(String[] args) {
        // Lambda expressions
        Operation add = (a, b) -> a + b;
        Operation multiply = (a, b) -> a * b;

        int sum = compute(5, 3, add);
        int product = compute(5, 3, multiply);
        int inline = compute(10, 2, (x, y) -> x - y);

        // Method references
        Function<String, Integer> parser = Integer::parseInt;
        int parsed = parser.apply("42");

        // Predicates
        Predicate<Integer> isPositive = n -> n > 0;
        Predicate<Integer> isEven = n -> n % 2 == 0;

        boolean positive = test(10, isPositive);
        boolean even = test(10, isEven);
        boolean combo = test(10, n -> n > 0 && n % 2 == 0);

        if (sum + product + inline + parsed > 0 && positive && even && combo) {
            compute(sum, product, add);
        }
    }
}
