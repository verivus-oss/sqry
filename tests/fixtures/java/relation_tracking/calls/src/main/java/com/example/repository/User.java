package com.example.repository;

import com.example.dto.UserDto;

public class User {
    private final Long id;

    public User(Long id) {
        this.id = id;
    }

    public UserDto toDto() {
        return new UserDto(id);
    }
}
