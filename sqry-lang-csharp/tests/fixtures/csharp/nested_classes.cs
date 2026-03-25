// Nested classes

namespace Demo.Nested {
    public class Outer {
        public void OuterMethod() {
            var inner = new Inner();
            inner.InnerMethod();
        }

        public class Inner {
            public void InnerMethod() {
                var deeplyNested = new DeeplyNested();
                deeplyNested.DeepMethod();
            }

            public class DeeplyNested {
                public void DeepMethod() {
                    // Deepest level
                }
            }
        }

        public void CallInnerFromOuter() {
            var inner = new Inner();
            var deep = new Inner.DeeplyNested();
            deep.DeepMethod();
        }
    }
}
