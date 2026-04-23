package com.example.enterprise;

public class Application {
    public static void main(String[] args) {
        UserRepository repository = new InMemoryUserRepository();
        UserService service = new UserService(repository);

        service.createUser(1L, "Alice", "alice@example.com");
        service.createUser(2L, "Bob", "bob@example.com");

        for (User user : service.listUsers()) {
            System.out.println(user.getName() + " <" + user.getEmail() + ">");
        }
    }
}
