<?php
/**
 * Namespaced Export Extraction Fixture
 *
 * Tests namespace-qualified exports and proper symbol naming.
 * Ground truth annotations mark expected exports with full namespace paths.
 */

namespace App\Http\Controllers;

// EXPORT: App\Http\Controllers\UserController
class UserController {
    // EXPORT: index
    public function index() {
        return [];
    }

    // EXPORT: show
    public function show($id) {
        return null;
    }

    // NOT EXPORTED: private
    private function authorize() {
        return true;
    }
}

namespace App\Services\Auth;

// EXPORT: App\Services\Auth\AuthenticationService
class AuthenticationService {
    // EXPORT: login
    public function login($username, $password) {
        return true;
    }

    // EXPORT: logout
    public function logout() {
        return true;
    }
}

// EXPORT: App\Services\Auth\SessionManager
class SessionManager {
    // EXPORT: start
    public function start() {
        session_start();
    }

    // EXPORT: destroy
    public function destroy() {
        session_destroy();
    }
}

namespace App\Models\User;

// EXPORT: App\Models\User\Profile
class Profile {
    // EXPORT: getFullName
    public function getFullName() {
        return '';
    }
}

namespace App\Repositories\Database;

// EXPORT: App\Repositories\Database\Connection
interface Connection {
    // EXPORT: execute
    public function execute($sql);

    // EXPORT: close
    public function close();
}

// EXPORT: App\Repositories\Database\MysqlConnection
class MysqlConnection implements Connection {
    // EXPORT: execute
    public function execute($sql) {
        return true;
    }

    // EXPORT: close
    public function close() {
        return true;
    }

    // NOT EXPORTED: private
    private function connect() {
        return null;
    }
}

namespace App\Helpers\String;

// EXPORT: App\Helpers\String\slugify (global function in namespace)
function slugify($text) {
    return strtolower(preg_replace('/[^a-z0-9]+/', '-', $text));
}

// EXPORT: App\Helpers\String\truncate (global function in namespace)
function truncate($text, $length = 100) {
    return strlen($text) > $length ? substr($text, 0, $length) . '...' : $text;
}

namespace App\Traits\Validation;

// EXPORT: App\Traits\Validation\ValidatesInput
trait ValidatesInput {
    // EXPORT: validate
    public function validate($rules) {
        return true;
    }

    // NOT EXPORTED: protected
    protected function checkRule($value, $rule) {
        return true;
    }
}
