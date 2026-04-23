package com.example.enterprise;

import java.util.List;
import java.util.Optional;

public class UserService {
    private final UserRepository repository;

    public UserService(UserRepository repository) {
        this.repository = repository;
    }

    public List<User> listUsers() {
        return repository.findAll();
    }

    public Optional<User> getUser(long id) {
        return repository.findById(id);
    }

    public User createUser(long id, String name, String email) {
        User user = new User(id, name, email);
        return repository.save(user);
    }

    public void removeUser(long id) {
        repository.deleteById(id);
    }
}
