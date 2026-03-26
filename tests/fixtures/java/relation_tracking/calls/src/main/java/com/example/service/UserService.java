package com.example.service;

import com.example.repository.User;
import com.example.repository.UserRepository;

public class UserService {
    private final UserRepository repository;

    public UserService(UserRepository repository) {
        this.repository = repository;
    }

    public User findById(Long id) {
        return repository.findById(id);
    }

    public void refresh() {
        repository.reset();
        UserRepository.clearCache();
    }
}
