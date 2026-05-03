<?php
// SPDX-License-Identifier: MIT

class InvoiceRecord {
    public string $mutableField;
    public readonly string $immutableField;
    public static int $staticField;
    private int $privateField;
    public int $sharedName;

    public function __construct(public string $promotedField, int $privateField) {
        $this->mutableField = $promotedField;
        $this->immutableField = "fixed";
        $this->privateField = $privateField;
    }
}

class AuditRecord {
    public int $sharedName;
}
