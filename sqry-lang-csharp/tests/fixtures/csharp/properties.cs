// Property declarations and access

namespace Demo.Props {
    public class User {
        // Auto-property
        public string Name { get; set; }

        // Read-only property
        public int Age { get; }

        // Property with explicit getter/setter
        private string _email;
        public string Email {
            get { return _email; }
            set { _email = value; }
        }

        public User(string name, int age) {
            Name = name;
            Age = age;
        }

        public void UpdateEmail(string newEmail) {
            Email = newEmail;
        }

        public string GetInfo() {
            return $"{Name} - {Age} - {Email}";
        }
    }

    public class Service {
        public User CreateUser() {
            var user = new User("Alice", 30);
            user.Email = "alice@example.com";
            return user;
        }

        public void DisplayUser(User user) {
            var name = user.Name;
            var age = user.Age;
            var email = user.Email;
        }
    }
}
