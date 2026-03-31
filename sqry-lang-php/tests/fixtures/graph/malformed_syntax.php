<?php

class BrokenClass {
    public function incomplete_method() {
        $var = "unclosed string
    }

    public function another_method() {
        return 42;
    }

    public function (broken receiver)->method_name() {
        // Invalid syntax
    }
}
