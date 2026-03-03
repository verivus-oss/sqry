//! CLI integration tests for Java relation queries
//!
//! Tests that relation queries work end-to-end through the CLI for Java:
//! - Callers queries (method calls, constructor calls, static calls)
//! - Callees queries (what a method calls)
//! - Exports queries (public classes, methods, interfaces)
//! - Imports queries (import statements)

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

// ============================================================================
// Exports Queries - Public Classes, Methods, Interfaces
// ============================================================================

#[test]
fn cli_java_exports_classes_and_methods() {
    let project = TempDir::new().unwrap();

    let java_code = r#"
public class User {
    private String name;

    public User(String name) {
        this.name = name;
    }

    public String getName() {
        return name;
    }

    private void validate() {
        // private method
    }
}
"#;
    std::fs::write(project.path().join("User.java"), java_code).unwrap();

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
        .stdout(predicate::str::contains("User.java"));

    // Query for public method
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:getName")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("User.java"));

    // Private validate method should not be exported
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:validate")
        .arg(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("No matches found"));
}

#[test]
fn cli_java_exports_interfaces() {
    let project = TempDir::new().unwrap();

    let java_code = r#"
public interface Repository<T> {
    void save(T item);
    T findById(int id);
    void delete(int id);
}

public class UserRepository implements Repository<User> {
    public void save(User user) {
        // implementation
    }

    public User findById(int id) {
        return null;
    }

    public void delete(int id) {
        // implementation
    }
}

class User {
    String name;
}
"#;
    std::fs::write(project.path().join("Repository.java"), java_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for interface
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:Repository")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Repository.java"));

    // Query for implementing class
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:UserRepository")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Repository.java"));
}

// ============================================================================
// Callers Queries - Method and Constructor Calls
// ============================================================================

#[test]
fn cli_java_callers_method_calls() {
    let project = TempDir::new().unwrap();

    let java_code = r#"
public class Processor {
    private boolean validate(String input) {
        return input != null && input.length() > 0;
    }

    public String process(String data) {
        if (validate(data)) {
            return data.trim();
        }
        return null;
    }

    public static void main(String[] args) {
        Processor p = new Processor();
        p.process("test");
    }
}
"#;
    std::fs::write(project.path().join("Processor.java"), java_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of validate
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:validate")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("process"));

    // Query for callers of process
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:process")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("main"));
}

#[test]
fn cli_java_callers_static_method_calls() {
    let project = TempDir::new().unwrap();

    let java_code = r#"
public class MathUtils {
    public static int add(int a, int b) {
        return a + b;
    }

    public static int multiply(int a, int b) {
        return a * b;
    }

    public static int calculate(int x, int y) {
        int sum = add(x, y);
        int product = multiply(x, y);
        return sum + product;
    }

    public static void main(String[] args) {
        int result = calculate(2, 3);
        System.out.println(result);
    }
}
"#;
    std::fs::write(project.path().join("MathUtils.java"), java_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of static methods
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:add")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("calculate"));

    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:multiply")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("calculate"));
}

#[test]
fn cli_java_callers_chained_method_calls() {
    let project = TempDir::new().unwrap();

    let java_code = r#"
public class QueryBuilder {
    private String condition;
    private String field;
    private int limit;

    public QueryBuilder where(String condition) {
        this.condition = condition;
        return this;
    }

    public QueryBuilder orderBy(String field) {
        this.field = field;
        return this;
    }

    public QueryBuilder limit(int count) {
        this.limit = count;
        return this;
    }

    public Object[] execute() {
        return new Object[0];
    }

    public static void main(String[] args) {
        runQuery();
    }

    private static Object[] runQuery() {
        return new QueryBuilder()
            .where("active = true")
            .orderBy("created_at")
            .limit(10)
            .execute();
    }
}
"#;
    std::fs::write(project.path().join("QueryBuilder.java"), java_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers in method chain
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:where")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("runQuery"));

    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:execute")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("runQuery"));
}

// ============================================================================
// Callees Queries - What Methods Call
// ============================================================================

#[test]
fn cli_java_callees_method() {
    let project = TempDir::new().unwrap();

    let java_code = r#"
public class Logger {
    private void log(String message) {
        System.out.println(message);
    }

    private void warn(String message) {
        System.out.println("WARNING: " + message);
    }

    public void handleError(Exception error) {
        log("Error occurred");
        warn(error.getMessage());
        System.err.println(error.getStackTrace());
    }

    public static void main(String[] args) {
        Logger logger = new Logger();
        logger.handleError(new Exception("Test"));
    }
}
"#;
    std::fs::write(project.path().join("Logger.java"), java_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callees of handleError
    Command::new(sqry_bin())
        .arg("query")
        .arg("callees:handleError")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("log"))
        .stdout(predicate::str::contains("warn"));
}

#[test]
fn cli_java_callees_with_inheritance() {
    let project = TempDir::new().unwrap();

    let java_code = r#"
class UserService {
    protected User findUser(int id) {
        return new User(id, "Test");
    }

    protected boolean validateUser(User user) {
        return user.getId() > 0;
    }

    public User getUser(int id) {
        User user = findUser(id);
        if (validateUser(user)) {
            return user;
        }
        return null;
    }
}

class User {
    private int id;
    private String name;

    public User(int id, String name) {
        this.id = id;
        this.name = name;
    }

    public int getId() {
        return id;
    }
}
"#;
    std::fs::write(project.path().join("UserService.java"), java_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callees of getUser
    Command::new(sqry_bin())
        .arg("query")
        .arg("callees:getUser")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("findUser"))
        .stdout(predicate::str::contains("validateUser"));
}

// ============================================================================
// Imports Queries - Package Imports
// ============================================================================

#[test]
fn cli_java_imports() {
    let project = TempDir::new().unwrap();

    let java_code = r#"
import java.util.List;
import java.util.ArrayList;
import java.io.File;
import java.io.IOException;

public class FileProcessor {
    private List<String> files;

    public FileProcessor() {
        files = new ArrayList<>();
    }

    public void processFile(String path) throws IOException {
        File file = new File(path);
        // process file
    }
}
"#;
    std::fs::write(project.path().join("FileProcessor.java"), java_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for imports
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:List")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("FileProcessor.java"));

    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:ArrayList")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("FileProcessor.java"));
}

// ============================================================================
// Generics and Advanced Features
// ============================================================================

#[test]
fn cli_java_generics() {
    let project = TempDir::new().unwrap();

    let java_code = r#"
public class DataService<T> {
    private T data;

    public void setData(T data) {
        this.data = data;
    }

    public T getData() {
        return data;
    }

    public T process(T input) {
        validate(input);
        return transform(input);
    }

    private void validate(T input) {
        // validation
    }

    private T transform(T input) {
        return input;
    }
}
"#;
    std::fs::write(project.path().join("DataService.java"), java_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callees of process
    Command::new(sqry_bin())
        .arg("query")
        .arg("callees:process")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("validate"))
        .stdout(predicate::str::contains("transform"));
}

// ============================================================================
// Negative Tests
// ============================================================================

#[test]
fn cli_java_private_methods() {
    let project = TempDir::new().unwrap();

    let java_code = r#"
public class Service {
    public void execute() {
        validate();
    }

    private void validate() {
        // private method
    }
}
"#;
    std::fs::write(project.path().join("Service.java"), java_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of private method
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:validate")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("execute"));
}

#[test]
fn cli_java_callers_no_results() {
    let project = TempDir::new().unwrap();

    let java_code = r#"
public class Unused {
    private int unusedMethod() {
        return 42;
    }

    public static void main(String[] args) {
        System.out.println("Hello");
    }
}
"#;
    std::fs::write(project.path().join("Unused.java"), java_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of unused method
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:unusedMethod")
        .arg(project.path())
        .assert()
        .success();
    // No specific assertion - just verify it doesn't crash
}
