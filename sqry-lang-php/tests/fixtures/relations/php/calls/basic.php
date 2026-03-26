<?php
/**
 * Basic Call Extraction Fixture
 *
 * Tests instance method calls, static method calls, and basic patterns.
 * Ground truth annotations mark expected call edges.
 */

namespace App\Services;

class UserService {
    private $repository;

    public function __construct(UserRepository $repository) {
        $this->repository = $repository;
    }

    // CALL: UserRepository::findById
    public function getUser($id) {
        return $this->repository->findById($id);
    }

    // CALL: UserRepository::save
    // CALL: UserService::hashPassword
    public function createUser($username, $password) {
        $hashedPassword = $this->hashPassword($password);
        $user = new \stdClass();
        $user->username = $username;
        $user->password = $hashedPassword;
        return $this->repository->save($user);
    }

    // CALL: password_hash
    private function hashPassword($password) {
        return password_hash($password, PASSWORD_DEFAULT);
    }

    // CALL: UserService::getUser
    // CALL: UserService::validateCredentials
    public function authenticate($username, $password) {
        $user = $this->getUser($username);
        if ($user) {
            return $this->validateCredentials($user, $password);
        }
        return false;
    }

    // CALL: password_verify
    private function validateCredentials($user, $password) {
        return password_verify($password, $user->password);
    }
}

class UserRepository {
    // CALL: Database::query
    public function findById($id) {
        return Database::query('SELECT * FROM users WHERE id = ?', [$id]);
    }

    // CALL: Database::insert
    public function save($user) {
        return Database::insert('users', $user);
    }
}

class Database {
    // CALL: self::connect
    // CALL: PDO::prepare
    public static function query($sql, $params = []) {
        $pdo = self::connect();
        $stmt = $pdo->prepare($sql);
        return $stmt;
    }

    // CALL: self::connect
    // CALL: PDO::prepare
    public static function insert($table, $data) {
        $pdo = self::connect();
        $sql = "INSERT INTO {$table} (username, password) VALUES (?, ?)";
        $stmt = $pdo->prepare($sql);
        return $stmt;
    }

    // CALL: PDO::__construct
    private static function connect() {
        $password = (string)getenv('SQRY_PHP_FIXTURE_DB_PASSWORD');
        return new \PDO('mysql:host=localhost;dbname=test', 'user', $password);
    }
}
