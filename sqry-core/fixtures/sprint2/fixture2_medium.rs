// Fixture 2: Medium synthetic (~1k LOC)
// Tests multiple modules with deeper call chains and type metadata

pub mod database {
    use std::collections::HashMap;

    pub struct Connection {
        pool: HashMap<String, String>,
    }

    impl Connection {
        pub fn new() -> Self {
            Self {
                pool: HashMap::new(),
            }
        }

        pub fn query(&self, sql: &str) -> Result<Vec<String>, String> {
            if sql.is_empty() {
                Err("Empty query".to_string())
            } else {
                Ok(vec!["row1".to_string(), "row2".to_string()])
            }
        }

        pub fn execute(&mut self, sql: &str) -> Result<usize, String> {
            if sql.starts_with("INSERT") {
                self.pool.insert(sql.to_string(), "executed".to_string());
                Ok(1)
            } else {
                Err("Invalid SQL".to_string())
            }
        }
    }

    pub fn connect(url: &str) -> Result<Connection, String> {
        if url.is_empty() {
            Err("Invalid URL".to_string())
        } else {
            Ok(Connection::new())
        }
    }

    pub fn disconnect(conn: Connection) -> bool {
        drop(conn);
        true
    }
}

pub mod models {
    #[derive(Debug, Clone)]
    pub struct User {
        pub id: u64,
        pub name: String,
        pub email: String,
    }

    impl User {
        pub fn new(id: u64, name: String, email: String) -> Self {
            Self { id, name, email }
        }

        pub fn validate(&self) -> Result<(), String> {
            if self.name.is_empty() {
                Err("Name required".to_string())
            } else if self.email.is_empty() {
                Err("Email required".to_string())
            } else {
                Ok(())
            }
        }

        pub fn to_string(&self) -> String {
            format!("{}: {} ({})", self.id, self.name, self.email)
        }
    }

    #[derive(Debug, Clone)]
    pub struct Post {
        pub id: u64,
        pub user_id: u64,
        pub title: String,
        pub content: String,
    }

    impl Post {
        pub fn new(id: u64, user_id: u64, title: String, content: String) -> Self {
            Self {
                id,
                user_id,
                title,
                content,
            }
        }

        pub fn validate(&self) -> Result<(), String> {
            if self.title.is_empty() {
                Err("Title required".to_string())
            } else {
                Ok(())
            }
        }

        pub fn summary(&self) -> String {
            format!("{}: {}", self.title, &self.content[..20.min(self.content.len())])
        }
    }
}

pub mod repository {
    use crate::database::{connect, Connection};
    use crate::models::{Post, User};

    pub struct UserRepository {
        conn: Connection,
    }

    impl UserRepository {
        pub fn new() -> Result<Self, String> {
            let conn = connect("postgres://localhost")?;
            Ok(Self { conn })
        }

        pub fn find_by_id(&self, id: u64) -> Result<Option<User>, String> {
            let sql = format!("SELECT * FROM users WHERE id = {}", id);
            let rows = self.conn.query(&sql)?;
            if rows.is_empty() {
                Ok(None)
            } else {
                Ok(Some(User::new(
                    id,
                    "John".to_string(),
                    "john@example.com".to_string(),
                )))
            }
        }

        pub fn find_all(&self) -> Result<Vec<User>, String> {
            let rows = self.conn.query("SELECT * FROM users")?;
            Ok(rows
                .iter()
                .enumerate()
                .map(|(i, _)| {
                    User::new(
                        i as u64,
                        format!("User{}", i),
                        format!("user{}@example.com", i),
                    )
                })
                .collect())
        }

        pub fn save(&mut self, user: &User) -> Result<u64, String> {
            user.validate()?;
            let sql = format!("INSERT INTO users VALUES ({}, '{}', '{}')", user.id, user.name, user.email);
            self.conn.execute(&sql)?;
            Ok(user.id)
        }

        pub fn delete(&mut self, id: u64) -> Result<bool, String> {
            let sql = format!("DELETE FROM users WHERE id = {}", id);
            self.conn.execute(&sql)?;
            Ok(true)
        }
    }

    pub struct PostRepository {
        conn: Connection,
    }

    impl PostRepository {
        pub fn new() -> Result<Self, String> {
            let conn = connect("postgres://localhost")?;
            Ok(Self { conn })
        }

        pub fn find_by_user(&self, user_id: u64) -> Result<Vec<Post>, String> {
            let sql = format!("SELECT * FROM posts WHERE user_id = {}", user_id);
            let rows = self.conn.query(&sql)?;
            Ok(rows
                .iter()
                .enumerate()
                .map(|(i, _)| {
                    Post::new(
                        i as u64,
                        user_id,
                        format!("Post {}", i),
                        "Content".to_string(),
                    )
                })
                .collect())
        }

        pub fn save(&mut self, post: &Post) -> Result<u64, String> {
            post.validate()?;
            let sql = format!("INSERT INTO posts VALUES ({}, {}, '{}', '{}')", post.id, post.user_id, post.title, post.content);
            self.conn.execute(&sql)?;
            Ok(post.id)
        }
    }
}

pub mod service {
    use crate::models::{Post, User};
    use crate::repository::{PostRepository, UserRepository};

    pub struct UserService {
        repo: UserRepository,
    }

    impl UserService {
        pub fn new() -> Result<Self, String> {
            Ok(Self {
                repo: UserRepository::new()?,
            })
        }

        pub fn get_user(&self, id: u64) -> Result<Option<User>, String> {
            self.repo.find_by_id(id)
        }

        pub fn list_users(&self) -> Result<Vec<User>, String> {
            self.repo.find_all()
        }

        pub fn create_user(&mut self, name: String, email: String) -> Result<u64, String> {
            let user = User::new(0, name, email);
            user.validate()?;
            self.repo.save(&user)
        }

        pub fn remove_user(&mut self, id: u64) -> Result<bool, String> {
            self.repo.delete(id)
        }
    }

    pub struct PostService {
        repo: PostRepository,
    }

    impl PostService {
        pub fn new() -> Result<Self, String> {
            Ok(Self {
                repo: PostRepository::new()?,
            })
        }

        pub fn get_posts_for_user(&self, user_id: u64) -> Result<Vec<Post>, String> {
            self.repo.find_by_user(user_id)
        }

        pub fn create_post(
            &mut self,
            user_id: u64,
            title: String,
            content: String,
        ) -> Result<u64, String> {
            let post = Post::new(0, user_id, title, content);
            post.validate()?;
            self.repo.save(&post)
        }
    }
}

pub mod api {
    use crate::models::User;
    use crate::service::{PostService, UserService};

    pub struct ApiHandler {
        user_service: UserService,
        post_service: PostService,
    }

    impl ApiHandler {
        pub fn new() -> Result<Self, String> {
            Ok(Self {
                user_service: UserService::new()?,
                post_service: PostService::new()?,
            })
        }

        pub fn handle_get_user(&self, id: u64) -> Result<Option<User>, String> {
            self.user_service.get_user(id)
        }

        pub fn handle_list_users(&self) -> Result<Vec<User>, String> {
            self.user_service.list_users()
        }

        pub fn handle_create_user(&mut self, name: String, email: String) -> Result<u64, String> {
            self.user_service.create_user(name, email)
        }

        pub fn handle_get_user_posts(&self, user_id: u64) -> Result<String, String> {
            let posts = self.post_service.get_posts_for_user(user_id)?;
            Ok(format!("Found {} posts", posts.len()))
        }

        pub fn handle_create_post(
            &mut self,
            user_id: u64,
            title: String,
            content: String,
        ) -> Result<u64, String> {
            self.post_service.create_post(user_id, title, content)
        }
    }

    pub fn router(path: &str) -> Result<String, String> {
        match path {
            "/users" => Ok("List users".to_string()),
            "/posts" => Ok("List posts".to_string()),
            _ => Err("Not found".to_string()),
        }
    }
}

pub mod utils {
    pub fn format_error(err: &str) -> String {
        format!("ERROR: {}", err)
    }

    pub fn validate_input(input: &str) -> Result<String, String> {
        if input.len() > 1000 {
            Err("Input too long".to_string())
        } else {
            Ok(input.to_string())
        }
    }

    pub fn sanitize(input: &str) -> String {
        input.replace("<", "&lt;").replace(">", "&gt;")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_user_creation() {
        let mut service = service::UserService::new().unwrap();
        let id = service
            .create_user("Test".to_string(), "test@example.com".to_string())
            .unwrap();
        assert_eq!(id, 0);
    }

    #[test]
    fn test_post_creation() {
        let mut service = service::PostService::new().unwrap();
        let id = service
            .create_post(1, "Title".to_string(), "Content".to_string())
            .unwrap();
        assert_eq!(id, 0);
    }

    #[test]
    fn test_api_handler() {
        let handler = api::ApiHandler::new().unwrap();
        let result = handler.handle_get_user(1);
        assert!(result.is_ok());
    }

    #[test]
    fn test_utils() {
        let result = utils::validate_input("test");
        assert!(result.is_ok());
    }
}
