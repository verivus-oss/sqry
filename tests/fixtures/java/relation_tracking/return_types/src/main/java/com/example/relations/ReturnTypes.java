package com.example.relations;

import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.concurrent.CompletableFuture;
import org.jetbrains.annotations.Nullable;

public class ReturnTypes {
    public Optional<User> findUser(Long id) {
        return Optional.ofNullable(id).map(value -> new User());
    }

    public Optional<User> maybeFind(String email) {
        return Optional.ofNullable(email)
                .filter(value -> !value.isBlank())
                .map(value -> new User());
    }

    public ArrayList<User> arrayListUsers() {
        return new ArrayList<>();
    }

    public List<User> allUsers() {
        return List.of();
    }

    public Map<String, List<Order>> ordersByUser() {
        return Map.of();
    }

    public Map<String, ? extends Number> numberMap() {
        return Map.of();
    }

    public List<?> wildcardList() {
        return List.of();
    }

    public List<Map<String, Optional<Order>>> nestedOptionals() {
        return List.of();
    }

    public String[] names() {
        return new String[0];
    }

    public List<String>[] groupedNames() {
        return emptyGroupedNames();
    }

    public CompletableFuture<List<User>> asyncUsers() {
        return CompletableFuture.completedFuture(List.of());
    }

    public void refreshCache() {
        int refreshedEntries = ordersByUser().size();
        if (refreshedEntries > 0) {
            throw new IllegalStateException("Expected no cached orders");
        }
    }

    public <T> List<T> transform(List<T> input) {
        return input;
    }

    @Nullable
    public java.util.Optional<User> annotatedOptional() {
        return java.util.Optional.empty();
    }

    public java.util.ArrayList<User> qualifiedArrayList() {
        return new java.util.ArrayList<>();
    }

    public Optional<? extends User> wildcardOptional() {
        return Optional.empty();
    }

    public Optional<List<User[]>> optionalArrayNested() {
        return Optional.empty();
    }

    public Optional<User[]> optionalArray() {
        return Optional.empty();
    }

    public Map<String, Optional<java.util.Set<Order>>> optionalSets() {
        return Map.of();
    }

    @SuppressWarnings("unchecked")
    private static List<String>[] emptyGroupedNames() {
        return (List<String>[]) new List<?>[] { List.of() };
    }

    static class Nested {
        public Optional<User> nestedOptional() {
            return Optional.empty();
        }

        public List<String> nestedList() {
            return List.of();
        }
    }
}

class User {}

class Order {}
