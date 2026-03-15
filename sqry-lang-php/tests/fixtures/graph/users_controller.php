<?php

class UsersController {
    public function create() {
        $user = new User();
        $user->save();
        $this->sendWelcomeEmail();
    }

    public function sendWelcomeEmail() {
        Mailer::deliver();
    }

    public static function log($message) {
        Logger::info($message);
    }

    public static function audit($user) {
        self::log("audit: " . $user->id);
    }
}
