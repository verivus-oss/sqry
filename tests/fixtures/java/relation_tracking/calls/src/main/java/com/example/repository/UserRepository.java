package com.example.repository;

public class UserRepository {
    public User findById(Long id) {
        return new User(id);
    }

    public void reset() {
        // reset underlying cache
    }

    public static void clearCache() {
        // clear static caches
    }
}
