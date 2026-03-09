package com.example.controller;

import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.PostMapping;
import org.springframework.web.bind.annotation.RequestMapping;
import org.springframework.web.bind.annotation.RestController;

@RestController
@RequestMapping("/api")
public class server_java {
    @GetMapping("/users")
    public String getUsers() {
        return "users";
    }

    @PostMapping("/items")
    public String createItem() {
        return "created";
    }
}
