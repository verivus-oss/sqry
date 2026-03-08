// Test fixture: Interfaces
// Tests: Interface method declarations, default methods

package com.example.interfaces;

public class Interfaces {

    interface Calculator {
        int add(int a, int b);

        int subtract(int a, int b);

        default int multiply(int a, int b) {
            return a * b;
        }

        static int divide(int a, int b) {
            if (b != 0) {
                return a / b;
            }
            return 0;
        }
    }

    static class SimpleCalculator implements Calculator {
        @Override
        public int add(int a, int b) {
            return a + b;
        }

        @Override
        public int subtract(int a, int b) {
            return a - b;
        }

        public int compute(int x, int y) {
            int sum = add(x, y);
            int product = multiply(x, y);
            return sum + product;
        }
    }

    public static void main(String[] args) {
        Calculator calc = new SimpleCalculator();
        int result1 = calc.add(5, 3);
        int result2 = calc.subtract(10, 4);
        int result3 = calc.multiply(6, 7);
        int result4 = Calculator.divide(20, 5);

        if (result1 + result2 + result3 + result4 > 0) {
            calc.add(result1, result4);
        }
    }
}
