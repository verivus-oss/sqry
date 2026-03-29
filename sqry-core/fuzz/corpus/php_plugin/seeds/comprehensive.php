<?php
namespace App\Services;

use App\Models\User;

class Calculator {
    private $value = 0;

    public function add(int $a, int $b): int {
        return $a + $b;
    }

    public static function multiply(int $a, int $b): int {
        return $a * $b;
    }

    final public function getValue(): int {
        return $this->value;
    }
}

trait Logger {
    public function log(string $message): void {
        echo $message;
    }
}
