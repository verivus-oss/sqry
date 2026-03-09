//! CLI integration tests for C# relation queries
//!
//! Tests that relation queries work end-to-end through the CLI for C#:
//! - Callers queries (method calls, property access, LINQ)
//! - Callees queries (what a method calls)
//! - Exports queries (public classes, methods, properties)
//! - Imports queries (using directives)

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

// ============================================================================
// Exports Queries - Public Classes, Methods, Properties
// ============================================================================

#[test]
fn cli_csharp_exports_classes_and_methods() {
    let project = TempDir::new().unwrap();

    let cs_code = r#"
using System;

namespace MyApp
{
    public class User
    {
        private string name;
        private int age;

        public User(string name, int age)
        {
            this.name = name;
            this.age = age;
        }

        public string GetName()
        {
            return name;
        }

        public int Age { get; set; }

        private void Validate()
        {
            // private method
        }
    }

    public static class Utils
    {
        public static string Greet(string name)
        {
            return $"Hello, {name}!";
        }
    }
}
"#;
    std::fs::write(project.path().join("User.cs"), cs_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for exported class
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:User")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("User.cs"));

    // Query for public method
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:GetName")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("User.cs"));

    // Query for static method
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:Greet")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("User.cs"));

    // Private Validate should not be exported
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:Validate")
        .arg(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("No matches found"));
}

#[test]
fn cli_csharp_exports_interfaces() {
    let project = TempDir::new().unwrap();

    let cs_code = r#"
using System;

namespace MyApp.Data
{
    public interface IRepository<T>
    {
        void Save(T item);
        T FindById(int id);
        void Delete(int id);
    }

    public class UserRepository : IRepository<User>
    {
        public void Save(User user)
        {
            // implementation
        }

        public User FindById(int id)
        {
            return null;
        }

        public void Delete(int id)
        {
            // implementation
        }
    }

    public class User
    {
        public int Id { get; set; }
        public string Name { get; set; }
    }
}
"#;
    std::fs::write(project.path().join("Repository.cs"), cs_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for interface
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:IRepository")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Repository.cs"));

    // Query for implementing class
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:UserRepository")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Repository.cs"));
}

// ============================================================================
// Callers Queries - Method Calls
// ============================================================================

#[test]
fn cli_csharp_callers_method_calls() {
    let project = TempDir::new().unwrap();

    let cs_code = r#"
using System;

namespace MyApp
{
    public class Processor
    {
        private bool Validate(string input)
        {
            return !string.IsNullOrEmpty(input);
        }

        public string Process(string data)
        {
            if (Validate(data))
            {
                return data.Trim();
            }
            return null;
        }

        public static void Main(string[] args)
        {
            var processor = new Processor();
            processor.Process("test");
        }
    }
}
"#;
    std::fs::write(project.path().join("Processor.cs"), cs_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of Validate
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:Validate")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Process"));

    // Query for callers of Process
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:Process")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Main"));
}

#[test]
fn cli_csharp_callers_fluent_interface() {
    let project = TempDir::new().unwrap();

    let cs_code = r#"
using System;
using System.Collections.Generic;

namespace MyApp
{
    public class QueryBuilder
    {
        private string condition;
        private string field;
        private int limit;

        public QueryBuilder Where(string condition)
        {
            this.condition = condition;
            return this;
        }

        public QueryBuilder OrderBy(string field)
        {
            this.field = field;
            return this;
        }

        public QueryBuilder Limit(int count)
        {
            this.limit = count;
            return this;
        }

        public List<object> Execute()
        {
            return new List<object>();
        }

        public static void Main()
        {
            RunQuery();
        }

        private static List<object> RunQuery()
        {
            return new QueryBuilder()
                .Where("active = true")
                .OrderBy("created_at")
                .Limit(10)
                .Execute();
        }
    }
}
"#;
    std::fs::write(project.path().join("QueryBuilder.cs"), cs_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers in fluent interface
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:Where")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("RunQuery"));

    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:Execute")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("RunQuery"));
}

// ============================================================================
// Callees Queries - What Methods Call
// ============================================================================

#[test]
fn cli_csharp_callees_method() {
    let project = TempDir::new().unwrap();

    let cs_code = r#"
using System;

namespace MyApp
{
    public class Logger
    {
        private void Log(string message)
        {
            Console.WriteLine(message);
        }

        private void Warn(string message)
        {
            Console.WriteLine($"WARNING: {message}");
        }

        public void HandleError(Exception error)
        {
            Log("Error occurred");
            Warn(error.Message);
            Console.Error.WriteLine(error.StackTrace);
        }

        public static void Main()
        {
            var logger = new Logger();
            logger.HandleError(new Exception("Test"));
        }
    }
}
"#;
    std::fs::write(project.path().join("Logger.cs"), cs_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callees of HandleError
    Command::new(sqry_bin())
        .arg("query")
        .arg("callees:HandleError")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Log"))
        .stdout(predicate::str::contains("Warn"));
}

// ============================================================================
// Imports Queries - Using Directives
// ============================================================================

#[test]
fn cli_csharp_imports() {
    let project = TempDir::new().unwrap();

    let cs_code = r#"
using System;
using System.IO;
using System.Collections.Generic;
using MyApp.Data;

namespace MyApp
{
    public class FileProcessor
    {
        private List<string> files;

        public FileProcessor()
        {
            files = new List<string>();
        }

        public void ProcessFile(string path)
        {
            var content = File.ReadAllText(path);
            Console.WriteLine(content);
        }
    }
}
"#;
    std::fs::write(project.path().join("FileProcessor.cs"), cs_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for using directives
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:System.IO")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("FileProcessor.cs"));

    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:System.Collections.Generic")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("FileProcessor.cs"));
}

// ============================================================================
// Properties, Events, and LINQ
// ============================================================================

#[test]
fn cli_csharp_linq() {
    let project = TempDir::new().unwrap();

    let cs_code = r#"
using System;
using System.Linq;
using System.Collections.Generic;

namespace MyApp
{
    public class DataProcessor
    {
        private int Transform(int x)
        {
            return x * 2;
        }

        public List<int> ProcessData()
        {
            var data = new List<int> { 1, 2, 3 };
            return data.Select(x => Transform(x)).ToList();
        }

        public static void Main()
        {
            var processor = new DataProcessor();
            processor.ProcessData();
        }
    }
}
"#;
    std::fs::write(project.path().join("DataProcessor.cs"), cs_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of Transform (used in LINQ)
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:Transform")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("ProcessData"));
}

// ============================================================================
// Async/Await
// ============================================================================

#[test]
fn cli_csharp_async_await() {
    let project = TempDir::new().unwrap();

    let cs_code = r#"
using System;
using System.Threading.Tasks;

namespace MyApp
{
    public class UserService
    {
        private async Task<User> FetchUser(int id)
        {
            await Task.Delay(100);
            return new User { Id = id, Name = "Test" };
        }

        private async Task<List<Post>> FetchPosts(int userId)
        {
            await Task.Delay(100);
            return new List<Post>();
        }

        public async Task<UserData> GetUserData(int id)
        {
            var user = await FetchUser(id);
            var posts = await FetchPosts(user.Id);
            return new UserData { User = user, Posts = posts };
        }
    }

    public class User
    {
        public int Id { get; set; }
        public string Name { get; set; }
    }

    public class Post
    {
        public int Id { get; set; }
        public string Title { get; set; }
    }

    public class UserData
    {
        public User User { get; set; }
        public List<Post> Posts { get; set; }
    }
}
"#;
    std::fs::write(project.path().join("UserService.cs"), cs_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callees of async method
    Command::new(sqry_bin())
        .arg("query")
        .arg("callees:GetUserData")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("FetchUser"))
        .stdout(predicate::str::contains("FetchPosts"));
}

// ============================================================================
// Negative Tests
// ============================================================================

#[test]
fn cli_csharp_private_methods() {
    let project = TempDir::new().unwrap();

    let cs_code = r#"
public class Service
{
    public void Execute()
    {
        Validate();
    }

    private void Validate()
    {
        // private method
    }
}
"#;
    std::fs::write(project.path().join("Service.cs"), cs_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of private method
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:Validate")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Execute"));
}

#[test]
fn cli_csharp_callers_no_results() {
    let project = TempDir::new().unwrap();

    let cs_code = r#"
public class Unused
{
    private int UnusedMethod()
    {
        return 42;
    }

    public static void Main()
    {
        System.Console.WriteLine("Hello");
    }
}
"#;
    std::fs::write(project.path().join("Unused.cs"), cs_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of unused method
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:UnusedMethod")
        .arg(project.path())
        .assert()
        .success();
    // No specific assertion - just verify it doesn't crash
}
