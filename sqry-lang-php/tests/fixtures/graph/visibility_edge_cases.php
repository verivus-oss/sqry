<?php

class VisibilityTest {
    public function public_method_1() {
        return 1;
    }

    private function private_method_1() {
        return 2;
    }

    protected function protected_method_1() {
        return 3;
    }

    private function private_method_2() {
        return 4;
    }

    // Static methods with varying visibility
    public static function public_class_method() {
        return 5;
    }

    private static function private_class_method() {
        return 6;
    }
}
