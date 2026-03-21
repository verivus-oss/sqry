<?php

class Crypto {
    private static $ffi;

    public static function setup() {
        self::$ffi = FFI::cdef("
            int crypto_encrypt(const char *data, int len);
        ", "libcrypto.so");
    }

    public static function encrypt($data) {
        return self::crypto_encrypt($data);
    }

    private static function crypto_encrypt($data) {
        return self::$ffi->crypto_encrypt($data, strlen($data));
    }
}
