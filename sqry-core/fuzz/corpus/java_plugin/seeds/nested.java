package com.a.b.c.d.e;

public class Outer {
    public static class Middle {
        public static class Inner {
            public static class DeepNested {
                public void method() {
                    new Runnable() {
                        public void run() {
                            class LocalClass {
                                void foo() { }
                            }
                        }
                    };
                }
            }
        }
    }
}
