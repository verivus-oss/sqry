// Static methods and members

namespace Demo.Static {
    public class MathUtils {
        public static int Add(int a, int b) {
            return a + b;
        }

        public static int Multiply(int a, int b) {
            return a * b;
        }

        public static int Calculate(int x, int y) {
            int sum = Add(x, y);
            int product = Multiply(x, y);
            return sum * product;
        }
    }

    public class Service {
        public int UseStaticMethods() {
            int result1 = MathUtils.Add(5, 3);
            int result2 = MathUtils.Multiply(2, 4);
            return result1 + result2;
        }

        // Instance method
        public int InstanceMethod(int x) {
            return MathUtils.Calculate(x, 10);
        }
    }
}
