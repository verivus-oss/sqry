<?php

class CryptoLibrary {
    private static $ffi;

    public static function init() {
        self::$ffi = FFI::cdef("
            int aes_encrypt(const char *data, int len);
            int aes_decrypt(const char *data, int len);
            char* sha256_hash(const char *data);
            char* sha512_hash(const char *data);
        ", "libcrypto.so");
    }

    public function encrypt($data) {
        return self::$ffi->aes_encrypt($data, strlen($data));
    }

    public function decrypt($data) {
        return self::$ffi->aes_decrypt($data, strlen($data));
    }
}

class CompressionLib {
    private static $compressor;

    public static function setup() {
        self::$compressor = FFI::load("/usr/lib/compression.h");
    }

    public function compress($data) {
        return self::$compressor->compress($data);
    }

    public function decompress($data) {
        return self::$compressor->decompress($data);
    }
}
