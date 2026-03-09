package com.example.controller;

import com.example.dto.UserDto;
import com.example.repository.User;
import com.example.service.UserService;

public class UserController {
    private final UserService service;

    public UserController(UserService service) {
        this.service = service;
    }

    public UserDto getUser(Long id) {
        User user = service.findById(id);
        return user.toDto();
    }
}
