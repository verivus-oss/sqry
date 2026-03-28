// Test fixture: Constructor calls
// Tests: new expressions, constructor chaining

package com.example.constructors;

public class ConstructorCalls {

    static class Point {
        private int x;
        private int y;

        public Point(int x, int y) {
            this.x = x;
            this.y = y;
        }

        public Point() {
            this(0, 0); // Constructor chaining
        }
    }

    static class Rectangle {
        private Point topLeft;
        private Point bottomRight;

        public Rectangle(Point topLeft, Point bottomRight) {
            this.topLeft = topLeft;
            this.bottomRight = bottomRight;
        }

        public Rectangle(int x1, int y1, int x2, int y2) {
            this(new Point(x1, y1), new Point(x2, y2));
        }
    }

    public static void main(String[] args) {
        Point p1 = new Point();
        Point p2 = new Point(10, 20);
        Rectangle rect = new Rectangle(p1, p2);
        Rectangle rect2 = new Rectangle(0, 0, 100, 100);

        if (rect != null && rect2 != null) {
            rect = rect2;
        }
    }
}
