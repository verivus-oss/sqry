<?php

class Ledger {
    public int $mutableField;
    public readonly int $immutableField;
    public static int $staticField;
    private string $privateField;
    public int $sharedName;

    public function __construct(public int $promotedField) {
        $this->mutableField = $promotedField;
        $this->immutableField = 1;
        $this->privateField = "";
    }
}

class Archive {
    public int $sharedName;
}
