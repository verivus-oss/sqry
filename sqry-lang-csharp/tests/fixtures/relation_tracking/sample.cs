using System.Threading.Tasks;
using static System.Math;
using Json = System.Text.Json.JsonSerializer;

namespace Demo.App {
    public static class Extensions {
        public static string TrimAll(this string value) => value.Trim();
    }

    public sealed class User {
        public string Name { get; }
        public User(string name = "anon") {
            Name = name;
        }
    }

    public static class Repository {
        public static Task<User> LoadAsync() => Task.FromResult(new User("repo"));
    }

    public class Service {
        public async Task<User> FetchAsync() {
            var user = await Repository.LoadAsync();
            return user;
        }

        public string Format(string input) {
            return input.TrimAll();
        }
    }
}
