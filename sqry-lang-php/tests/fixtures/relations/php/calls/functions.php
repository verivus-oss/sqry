<?php
/**
 * Function Call Extraction Fixture
 *
 * Tests global function calls, namespaced function calls, and built-in functions.
 * Ground truth annotations mark expected call edges.
 */

namespace App\Helpers;

// CALL: strlen
// CALL: trim
// CALL: strtolower
function normalizeUsername($username) {
    $trimmed = trim($username);
    $lowercase = strtolower($trimmed);
    if (strlen($lowercase) < 3) {
        return null;
    }
    return $lowercase;
}

// CALL: preg_match
// CALL: filter_var
function validateEmail($email) {
    if (!filter_var($email, FILTER_VALIDATE_EMAIL)) {
        return false;
    }
    return preg_match('/^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$/', $email);
}

// CALL: hash
// CALL: random_bytes
// CALL: bin2hex
function generateToken($data) {
    $salt = bin2hex(random_bytes(16));
    return hash('sha256', $data . $salt);
}

namespace App\Utils;

// CALL: App\Helpers\normalizeUsername
// CALL: App\Helpers\validateEmail
function processUserInput($username, $email) {
    $normalized = \App\Helpers\normalizeUsername($username);
    $valid = \App\Helpers\validateEmail($email);
    return $normalized && $valid;
}

// CALL: json_encode
// CALL: file_put_contents
function saveToFile($data, $path) {
    $json = json_encode($data, JSON_PRETTY_PRINT);
    return file_put_contents($path, $json);
}

// CALL: file_get_contents
// CALL: json_decode
function loadFromFile($path) {
    $content = file_get_contents($path);
    return json_decode($content, true);
}

namespace App;

// CALL: App\Helpers\generateToken
// CALL: App\Utils\saveToFile
function createSession($userId) {
    $token = Helpers\generateToken($userId);
    Utils\saveToFile(['user_id' => $userId, 'token' => $token], '/tmp/session.json');
    return $token;
}

// Global namespace function calls

// CALL: time
// CALL: date
// CALL: strtotime
function formatTimestamp($timestamp) {
    if (is_string($timestamp)) {
        $timestamp = strtotime($timestamp);
    }
    return date('Y-m-d H:i:s', $timestamp);
}

// CALL: array_map
// CALL: array_filter
// CALL: count
function processArray($items) {
    $filtered = array_filter($items, function($item) {
        return $item !== null;
    });
    $mapped = array_map(function($item) {
        return strtoupper($item);
    }, $filtered);
    return count($mapped);
}
