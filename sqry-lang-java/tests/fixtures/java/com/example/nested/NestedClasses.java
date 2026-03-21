// Test fixture: Nested classes
// Tests: Outer.Inner.method() qualification

package com.example.nested;

public class NestedClasses {

    private String outerField;

    public NestedClasses(String outerField) {
        this.outerField = outerField;
    }

    public void outerMethod() {
        outerMethod(true);
    }

    private void outerMethod(boolean callInner) {
        if (!callInner) {
            return;
        }
        Inner inner = new Inner("inner");
        inner.innerMethod(false);
    }

    public class Inner {
        private String innerField;

        public Inner(String innerField) {
            this.innerField = innerField;
        }

        public void innerMethod() {
            innerMethod(true);
        }

        private void innerMethod(boolean callOuter) {
            System.out.println(outerField + " - " + innerField);
            if (callOuter) {
                outerMethod(false);
            }
        }

        public class DeepNested {
            public void deepMethod() {
                innerMethod();
                outerMethod();
            }
        }
    }

    public static class StaticNested {
        private String staticField;

        public StaticNested(String staticField) {
            this.staticField = staticField;
        }

        public void staticNestedMethod() {
            System.out.println(staticField);
        }
    }

    public static void main(String[] args) {
        NestedClasses outer = new NestedClasses("outer");
        outer.outerMethod();

        NestedClasses.Inner inner = outer.new Inner("test");
        inner.innerMethod();

        NestedClasses.StaticNested staticNested = new NestedClasses.StaticNested("static");
        staticNested.staticNestedMethod();
    }
}
