<?php
/**
 * Method Chaining Call Extraction Fixture
 *
 * Tests method chaining patterns, fluent interfaces, and cascading calls.
 * Ground truth annotations mark expected call edges.
 */

namespace App\Builders;

class QueryBuilder {
    private $table;
    private $wheres = [];
    private $orderBy;

    public function table($table) {
        $this->table = $table;
        return $this;
    }

    public function where($column, $value) {
        $this->wheres[] = [$column, $value];
        return $this;
    }

    public function orderBy($column) {
        $this->orderBy = $column;
        return $this;
    }

    // CALL: implode
    // CALL: Database::execute
    public function get() {
        $sql = "SELECT * FROM {$this->table}";
        if (!empty($this->wheres)) {
            $conditions = implode(' AND ', array_map(function($w) {
                return "{$w[0]} = ?";
            }, $this->wheres));
            $sql .= " WHERE {$conditions}";
        }
        if ($this->orderBy) {
            $sql .= " ORDER BY {$this->orderBy}";
        }
        return Database::execute($sql);
    }
}

class UserQuery {
    private $builder;

    public function __construct() {
        $this->builder = new QueryBuilder();
    }

    // CALL: QueryBuilder::table
    // CALL: QueryBuilder::where
    // CALL: QueryBuilder::orderBy
    // CALL: QueryBuilder::get
    public function findActiveUsers() {
        return $this->builder
            ->table('users')
            ->where('status', 'active')
            ->orderBy('created_at')
            ->get();
    }

    // CALL: QueryBuilder::table
    // CALL: QueryBuilder::where
    // CALL: QueryBuilder::where (second call)
    // CALL: QueryBuilder::get
    public function findUserByEmail($email) {
        return $this->builder
            ->table('users')
            ->where('email', $email)
            ->where('deleted_at', null)
            ->get();
    }
}

namespace App\Services;

class FluentLogger {
    private $level;
    private $message;
    private $context = [];

    public function level($level) {
        $this->level = $level;
        return $this;
    }

    public function message($message) {
        $this->message = $message;
        return $this;
    }

    public function context(array $context) {
        $this->context = $context;
        return $this;
    }

    // CALL: json_encode
    // CALL: file_put_contents
    public function write() {
        $entry = json_encode([
            'level' => $this->level,
            'message' => $this->message,
            'context' => $this->context,
            'timestamp' => time(),
        ]);
        file_put_contents('/tmp/app.log', $entry . "\n", FILE_APPEND);
        return $this;
    }
}

class LogManager {
    private $logger;

    public function __construct() {
        $this->logger = new FluentLogger();
    }

    // CALL: FluentLogger::level
    // CALL: FluentLogger::message
    // CALL: FluentLogger::context
    // CALL: FluentLogger::write
    public function logError($message, $context = []) {
        $this->logger
            ->level('error')
            ->message($message)
            ->context($context)
            ->write();
    }

    // CALL: FluentLogger::level
    // CALL: FluentLogger::message
    // CALL: FluentLogger::write
    public function logInfo($message) {
        $this->logger->level('info')->message($message)->write();
    }
}
