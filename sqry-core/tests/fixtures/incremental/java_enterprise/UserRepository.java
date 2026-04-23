package com.example.enterprise;

import java.util.List;
import java.util.Optional;

public interface UserRepository {
    List<User> findAll();
    Optional<User> findById(long id);
    User save(User user);
    void deleteById(long id);
}
