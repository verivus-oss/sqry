<?php

namespace Level1\Level2\Level3 {
    class Level4Class {
        public function method1() {
            return 1;
        }
    }
}

namespace Level1\Level2\Level3\Level4\Level5 {
    class DeepClass {
        public function deepMethod() {
            return 2;
        }
    }
}

namespace Shallow {
    class TestClass {
        public function test() {
            return 3;
        }
    }
}
