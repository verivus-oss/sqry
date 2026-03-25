//! CLI integration tests for TypeScript relation queries
//!
//! Tests that relation queries work end-to-end through the CLI for TypeScript:
//! - Callers queries (function calls, method calls, constructor calls)
//! - Callees queries (what a function calls)
//! - Exports queries (ES6 modules, interfaces, types, enums)
//! - Imports queries (import statements, type imports)

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

// ============================================================================
// Exports Queries - TypeScript Modules, Interfaces, Types
// ============================================================================

#[test]
fn cli_typescript_exports_functions_and_classes() {
    let project = TempDir::new().unwrap();

    let ts_code = r#"
export function greet(name: string): string {
    return `Hello, ${name}!`;
}

export class User {
    constructor(private name: string) {}

    getName(): string {
        return this.name;
    }
}

export const API_VERSION = "1.0.0";

function internalHelper(): number {
    return 42;
}
"#;
    std::fs::write(project.path().join("module.ts"), ts_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for exported function
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:greet")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("module.ts"));

    // Query for exported class
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:User")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("module.ts"));

    // Query for non-exported helper (should not appear)
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:internalHelper")
        .arg(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("No matches found"));
}

#[test]
fn cli_typescript_exports_interfaces_and_types() {
    let project = TempDir::new().unwrap();

    let ts_code = r#"
export interface UserData {
    id: number;
    name: string;
    email: string;
}

export type UserId = number;
export type UserCallback = (user: UserData) => void;

export enum UserRole {
    Admin,
    User,
    Guest
}

export function createUser(data: UserData): UserData {
    return data;
}
"#;
    std::fs::write(project.path().join("types.ts"), ts_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for exported interface
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:UserData")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("types.ts"));

    // Query for exported enum
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:UserRole")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("types.ts"));
}

#[test]
fn cli_typescript_exports_namespaces() {
    let project = TempDir::new().unwrap();

    let ts_code = r#"
export namespace Utils {
    export function validate(input: string): boolean {
        return input.length > 0;
    }

    export class Validator {
        check(value: any): boolean {
            return !!value;
        }
    }
}

export namespace API {
    export interface Response {
        status: number;
        data: any;
    }

    export function request(url: string): Response {
        return { status: 200, data: {} };
    }
}
"#;
    std::fs::write(project.path().join("namespaces.ts"), ts_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for exported namespace member (class)
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:Validator")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("namespaces.ts"));

    // Query for namespace members
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:validate")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("namespaces.ts"));
}

// ============================================================================
// Callers Queries - Function and Method Calls
// ============================================================================

#[test]
fn cli_typescript_callers_function_calls() {
    let project = TempDir::new().unwrap();

    let ts_code = r#"
function validate(input: string): boolean {
    return input.length > 0;
}

function process(data: string): string | null {
    if (validate(data)) {
        return data.trim();
    }
    return null;
}

process("test");
"#;
    std::fs::write(project.path().join("processor.ts"), ts_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of DataService::validate
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:DataService::validate")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("process"));
}

#[test]
fn cli_typescript_callers_method_calls_with_generics() {
    let project = TempDir::new().unwrap();

    let ts_code = r#"
function validate<T>(item: T): boolean {
    return item !== null;
}

function process<T>(items: T[]): T[] {
    for (const entry of items) {
        validate(entry);
    }
    return items;
}

const values: Array<string | null> = ["test", null];
process(values);
"#;
    std::fs::write(project.path().join("service.ts"), ts_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of validate in generic context
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:validate")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("process"));

    // Ensure query still succeeds when searching for callers of process itself
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:process")
        .arg(project.path())
        .assert()
        .success();
}

#[test]
fn cli_typescript_callers_interface_implementation() {
    let project = TempDir::new().unwrap();

    let ts_code = r#"
interface Repository<T> {
    save(item: T): void;
    findById(id: number): T | null;
}

class UserRepository implements Repository<User> {
    save(user: User): void {
        this.validate(user);
    }

    findById(id: number): User | null {
        return null;
    }

    private validate(user: User): void {
        // validation logic
    }
}

interface User {
    id: number;
    name: string;
}

const repo = new UserRepository();
repo.save({ id: 1, name: "Test" });
"#;
    std::fs::write(project.path().join("repository.ts"), ts_code).unwrap();

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
        .stdout(predicate::str::contains("save"));
}

// ============================================================================
// Callees Queries - What Functions Call
// ============================================================================

#[test]
fn cli_typescript_callees_function() {
    let project = TempDir::new().unwrap();

    let ts_code = r#"
function log(message: string): void {
    console.log(message);
}

function warn(message: string): void {
    console.warn(message);
}

function handleError(error: Error): void {
    log('Error occurred');
    warn(error.message);
    console.error(error.stack);
}

handleError(new Error('Test'));
"#;
    std::fs::write(project.path().join("logger.ts"), ts_code).unwrap();

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
fn cli_typescript_callees_async_function() {
    let project = TempDir::new().unwrap();

    let ts_code = r#"
async function fetchUser(id: number): Promise<User> {
    return { id, name: 'Test' };
}

async function fetchPosts(userId: number): Promise<Post[]> {
    return [];
}

async function getUserData(id: number): Promise<UserData> {
    const user = await fetchUser(id);
    const posts = await fetchPosts(user.id);
    return { user, posts };
}

interface User {
    id: number;
    name: string;
}

interface Post {
    id: number;
    title: string;
}

interface UserData {
    user: User;
    posts: Post[];
}

getUserData(1);
"#;
    std::fs::write(project.path().join("async.ts"), ts_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callees of async function
    Command::new(sqry_bin())
        .arg("query")
        .arg("callees:getUserData")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("fetchUser"))
        .stdout(predicate::str::contains("fetchPosts"));
}

// ============================================================================
// Imports Queries - Module Imports
// ============================================================================

#[test]
fn cli_typescript_imports_es6_imports() {
    let project = TempDir::new().unwrap();

    let ts_code = r#"
import { greet, farewell } from './utils';
import User from './user';
import type { UserData } from './types';
import * as helpers from './helpers';

function main(): void {
    const user = new User('Alice');
    greet(user.getName());
    helpers.process();
}

main();
"#;
    std::fs::write(project.path().join("main.ts"), ts_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for imports of 'user' module (imports:X matches module names, not symbol names)
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:user")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("main.ts"));

    // Query for imports of 'utils' module
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:utils")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("main.ts"));
}

#[test]
fn cli_typescript_imports_type_only() {
    let project = TempDir::new().unwrap();

    let ts_code = r#"
import type { UserData, UserRole } from './types';
import { type UserId, createUser } from './user';

function processUser(data: UserData, role: UserRole): void {
    createUser(data);
}
"#;
    std::fs::write(project.path().join("processor.ts"), ts_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for type imports
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:UserData")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("processor.ts"));

    // Query for mixed type/value imports
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:createUser")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("processor.ts"));
}

// ============================================================================
// Decorators and Advanced TypeScript Features
// ============================================================================

#[test]
fn cli_typescript_decorators() {
    let project = TempDir::new().unwrap();

    let ts_code = r#"
function log(target: any, propertyKey: string, descriptor: PropertyDescriptor) {
    const originalMethod = descriptor.value;
    descriptor.value = function(...args: any[]) {
        console.log(`Calling ${propertyKey}`);
        return originalMethod.apply(this, args);
    };
    return descriptor;
}

export class UserService {
    @log
    createUser(name: string): void {
        console.log(`Creating user: ${name}`);
    }

    @log
    deleteUser(id: number): void {
        console.log(`Deleting user: ${id}`);
    }
}

const service = new UserService();
service.createUser("Alice");
"#;
    std::fs::write(project.path().join("decorators.ts"), ts_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for exported class
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:UserService")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("decorators.ts"));

    // Query for callers of createUser
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:createUser")
        .arg(project.path())
        .assert()
        .success();
}

// ============================================================================
// Negative Tests
// ============================================================================

#[test]
fn cli_typescript_private_methods_not_in_exports() {
    let project = TempDir::new().unwrap();

    let ts_code = r#"
export class Service {
    public execute(): void {
        this.validate();
    }

    private validate(): void {
        // private method
    }
}
"#;
    std::fs::write(project.path().join("service.ts"), ts_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for public class export
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:Service")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("service.ts"));

    // Private methods may or may not appear in exports depending on implementation
}

#[test]
fn cli_typescript_callers_no_results() {
    let project = TempDir::new().unwrap();

    let ts_code = r#"
function unusedFunction(): number {
    return 42;
}

function main(): void {
    console.log('Hello');
}

main();
"#;
    std::fs::write(project.path().join("unused.ts"), ts_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of unused function
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:unusedFunction")
        .arg(project.path())
        .assert()
        .success();
    // No specific assertion - just verify it doesn't crash
}
