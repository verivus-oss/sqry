// Constructor calls and chaining

namespace Demo.Ctors {
    public class Point {
        public int X { get; }
        public int Y { get; }

        public Point(int x, int y) {
            X = x;
            Y = y;
        }

        public Point() : this(0, 0) {
            // Delegates to other constructor
        }
    }

    public class Factory {
        public Point CreateOrigin() {
            return new Point();
        }

        public Point CreatePoint(int x, int y) {
            return new Point(x, y);
        }

        public void CreateMultiple() {
            var p1 = new Point(1, 2);
            var p2 = new Point(3, 4);
            var p3 = new Point();
        }
    }
}
