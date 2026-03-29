<?php
/**
 * Visibility Filtering Export Fixture
 *
 * Tests visibility modifiers (public, protected, private) and implicit visibility.
 * Ground truth annotations mark expected exports (only public methods).
 */

namespace App\Services;

// EXPORT: UserService
class UserService {
    private $repository;

    // EXPORT: __construct (public constructor)
    public function __construct($repository) {
        $this->repository = $repository;
    }

    // EXPORT: createUser (explicitly public)
    public function createUser($username, $email, $password) {
        $this->validateInput($username, $email, $password);
        $hashedPassword = $this->hashPassword($password);
        return $this->repository->save([
            'username' => $username,
            'email' => $email,
            'password' => $hashedPassword,
        ]);
    }

    // EXPORT: updateUser (explicitly public)
    public function updateUser($id, $data) {
        $existing = $this->repository->findById($id);
        if (!$existing) {
            return false;
        }
        $this->auditUpdate($id, $data);
        return $this->repository->update($id, $data);
    }

    // EXPORT: deleteUser (explicitly public)
    public function deleteUser($id) {
        $this->checkDeletionPermissions($id);
        return $this->repository->delete($id);
    }

    // EXPORT: getUser (explicitly public)
    public function getUser($id) {
        return $this->repository->findById($id);
    }

    // NOT EXPORTED: protected method
    protected function validateInput($username, $email, $password) {
        if (strlen($username) < 3) {
            throw new \InvalidArgumentException('Username too short');
        }
        if (!filter_var($email, FILTER_VALIDATE_EMAIL)) {
            throw new \InvalidArgumentException('Invalid email');
        }
        if (strlen($password) < 8) {
            throw new \InvalidArgumentException('Password too short');
        }
    }

    // NOT EXPORTED: private method
    private function hashPassword($password) {
        return password_hash($password, PASSWORD_DEFAULT);
    }

    // NOT EXPORTED: protected method
    protected function auditUpdate($id, $data) {
        error_log("User {$id} updated with data: " . json_encode($data));
    }

    // NOT EXPORTED: private method
    private function checkDeletionPermissions($id) {
        // Check if current user can delete user with $id
    }
}

// EXPORT: AbstractRepository
abstract class AbstractRepository {
    // EXPORT: findById (public in abstract class)
    public function findById($id) {
        return $this->query("SELECT * FROM {$this->getTableName()} WHERE id = ?", [$id]);
    }

    // EXPORT: findAll (public in abstract class)
    public function findAll() {
        return $this->query("SELECT * FROM {$this->getTableName()}");
    }

    // NOT EXPORTED: protected abstract method
    abstract protected function getTableName();

    // NOT EXPORTED: protected method
    protected function query($sql, $params = []) {
        // Execute query
        return [];
    }

    // NOT EXPORTED: private method
    private function buildConnection() {
        return null;
    }
}

// EXPORT: ProductRepository
class ProductRepository extends AbstractRepository {
    // EXPORT: save (public method)
    public function save($product) {
        $sql = "INSERT INTO products (name, price) VALUES (?, ?)";
        return $this->query($sql, [$product['name'], $product['price']]);
    }

    // NOT EXPORTED: protected method implementing abstract
    protected function getTableName() {
        return 'products';
    }

    // NOT EXPORTED: private method
    private function validateProduct($product) {
        return isset($product['name']) && isset($product['price']);
    }
}

// EXPORT: Logger
class Logger {
    // EXPORT: info (no visibility modifier defaults to public)
    function info($message) {
        $this->write('INFO', $message);
    }

    // EXPORT: error (no visibility modifier defaults to public)
    function error($message) {
        $this->write('ERROR', $message);
    }

    // EXPORT: warning (explicit public)
    public function warning($message) {
        $this->write('WARNING', $message);
    }

    // NOT EXPORTED: private method
    private function write($level, $message) {
        error_log("[{$level}] {$message}");
    }

    // NOT EXPORTED: protected method
    protected function formatMessage($level, $message) {
        return sprintf('[%s] %s - %s', date('Y-m-d H:i:s'), $level, $message);
    }
}
