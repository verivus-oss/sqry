<?php
/**
 * Trait Export Extraction Fixture
 *
 * Tests trait definitions and their exported methods.
 * Ground truth annotations mark expected exports.
 */

namespace App\Traits;

// EXPORT: Timestampable
trait Timestampable {
    protected $created_at;
    protected $updated_at;

    // EXPORT: getCreatedAt
    public function getCreatedAt() {
        return $this->created_at;
    }

    // EXPORT: getUpdatedAt
    public function getUpdatedAt() {
        return $this->updated_at;
    }

    // EXPORT: touch
    public function touch() {
        $this->updated_at = time();
    }

    // NOT EXPORTED: protected method
    protected function initializeTimestamps() {
        $this->created_at = time();
        $this->updated_at = time();
    }

    // NOT EXPORTED: private method
    private function formatTimestamp($timestamp) {
        return date('Y-m-d H:i:s', $timestamp);
    }
}

// EXPORT: SoftDeletes
trait SoftDeletes {
    protected $deleted_at;

    // EXPORT: delete (soft delete)
    public function delete() {
        $this->deleted_at = time();
    }

    // EXPORT: restore
    public function restore() {
        $this->deleted_at = null;
    }

    // EXPORT: isDeleted
    public function isDeleted() {
        return $this->deleted_at !== null;
    }

    // NOT EXPORTED: protected method
    protected function forceDelete() {
        // Actually delete from database
    }
}

// EXPORT: Loggable
trait Loggable {
    // EXPORT: log
    public function log($message, $level = 'info') {
        $this->writeLog($level, $message);
    }

    // EXPORT: logError
    public function logError($message) {
        $this->log($message, 'error');
    }

    // EXPORT: logWarning
    public function logWarning($message) {
        $this->log($message, 'warning');
    }

    // NOT EXPORTED: private helper
    private function writeLog($level, $message) {
        error_log("[{$level}] {$message}");
    }
}

namespace App\Models;

use App\Traits\Timestampable;
use App\Traits\SoftDeletes;
use App\Traits\Loggable;

// EXPORT: Post
class Post {
    use Timestampable, SoftDeletes, Loggable;

    private $title;
    private $content;

    // EXPORT: __construct
    public function __construct($title, $content) {
        $this->title = $title;
        $this->content = $content;
        $this->initializeTimestamps();
    }

    // EXPORT: getTitle
    public function getTitle() {
        return $this->title;
    }

    // EXPORT: getContent
    public function getContent() {
        return $this->content;
    }

    // NOT EXPORTED: private method
    private function sanitizeContent($content) {
        return htmlspecialchars($content);
    }
}
