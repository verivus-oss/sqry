<?php
/**
 * Class and Interface Export Extraction Fixture
 *
 * Tests class definitions, interface definitions, and their exported symbols.
 * Ground truth annotations mark expected exports.
 */

namespace App\Models;

// EXPORT: User
class User {
    private $id;
    private $username;
    private $email;

    // EXPORT: __construct
    public function __construct($username, $email) {
        $this->username = $username;
        $this->email = $email;
    }

    // EXPORT: getId
    public function getId() {
        return $this->id;
    }

    // EXPORT: getUsername
    public function getUsername() {
        return $this->username;
    }

    // EXPORT: getEmail
    public function getEmail() {
        return $this->email;
    }

    // NOT EXPORTED: private method
    private function hashPassword($password) {
        return password_hash($password, PASSWORD_DEFAULT);
    }

    // NOT EXPORTED: protected method
    protected function validateEmail($email) {
        return filter_var($email, FILTER_VALIDATE_EMAIL);
    }
}

// EXPORT: Repository
interface Repository {
    // EXPORT: findById (interface methods are implicitly public)
    public function findById($id);

    // EXPORT: save
    public function save($entity);

    // EXPORT: delete
    public function delete($id);

    // EXPORT: findAll
    public function findAll();
}

// EXPORT: UserRepository
class UserRepository implements Repository {
    private $connection;

    // EXPORT: __construct
    public function __construct($connection) {
        $this->connection = $connection;
    }

    // EXPORT: findById (implementing interface method)
    public function findById($id) {
        return $this->executeQuery("SELECT * FROM users WHERE id = ?", [$id]);
    }

    // EXPORT: save (implementing interface method)
    public function save($entity) {
        return $this->executeQuery("INSERT INTO users VALUES (?, ?)", [$entity->username, $entity->email]);
    }

    // EXPORT: delete (implementing interface method)
    public function delete($id) {
        return $this->executeQuery("DELETE FROM users WHERE id = ?", [$id]);
    }

    // EXPORT: findAll (implementing interface method)
    public function findAll() {
        return $this->executeQuery("SELECT * FROM users");
    }

    // NOT EXPORTED: private helper
    private function executeQuery($sql, $params = []) {
        return null;
    }
}

namespace App\Contracts;

// EXPORT: Authenticatable
interface Authenticatable {
    // EXPORT: authenticate
    public function authenticate($credentials);

    // EXPORT: logout
    public function logout();

    // EXPORT: isAuthenticated
    public function isAuthenticated();
}

// EXPORT: Cacheable
interface Cacheable {
    // EXPORT: getCacheKey
    public function getCacheKey();

    // EXPORT: getCacheDuration
    public function getCacheDuration();
}
