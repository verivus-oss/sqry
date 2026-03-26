// Simple method calls

namespace Demo {
    public class Calculator {
        public int Add(int a, int b) {
            return a + b;
        }

        public int Multiply(int a, int b) {
            return a * b;
        }

        public int Calculate(int x, int y) {
            int sum = Add(x, y);
            int product = Multiply(x, y);
            return sum + product;
        }
    }
}
