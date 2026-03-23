// RKG: CODE:RELATIONS-SHARED implements REQ:SQRY-RUBY-QUALIFIED-CALLERS
use serde::{Deserialize, Serialize};

/// Method kind for canonical caller identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CallIdentityKind {
    /// Instance method (`Namespace::Class#method`).
    #[default]
    Instance,
    /// Singleton/class method defined as `def self.method` or similar.
    Singleton,
    /// Methods defined inside `class << self` blocks.
    SingletonClass,
}

/// Structured metadata describing a caller.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CallIdentityMetadata {
    /// Fully qualified caller identifier (e.g., `Admin::Users::Controller#show`).
    pub qualified: String,
    /// Simple caller name (e.g., `show`).
    pub simple: String,
    /// Namespace stack (modules/classes) enclosing the method.
    pub namespace: Vec<String>,
    /// Method kind (instance, singleton, singleton-class).
    pub method_kind: CallIdentityKind,
    /// Optional receiver expression captured at call site.
    #[serde(default)]
    pub receiver: Option<String>,
}

/// Builder used by language adapters to construct canonical caller identities.
#[derive(Debug, Default)]
pub struct CallIdentityBuilder {
    namespace: Vec<String>,
    simple: String,
    kind: CallIdentityKind,
    receiver: Option<String>,
}

impl CallIdentityBuilder {
    /// Create a new builder for the provided method name/kind.
    #[must_use]
    pub fn new(simple: impl Into<String>, kind: CallIdentityKind) -> Self {
        Self {
            namespace: Vec::new(),
            simple: simple.into(),
            kind,
            receiver: None,
        }
    }

    /// Replace the namespace stack with the provided iterator.
    #[must_use]
    pub fn with_namespace<I, S>(mut self, segments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.namespace = segments.into_iter().map(Into::into).collect();
        self
    }

    /// Add a single namespace segment (e.g., module or class name).
    #[must_use]
    pub fn push_namespace(mut self, segment: impl Into<String>) -> Self {
        self.namespace.push(segment.into());
        self
    }

    /// Attach an explicit receiver expression (used for diagnostics/metadata).
    #[must_use]
    pub fn with_receiver(mut self, receiver: impl Into<String>) -> Self {
        self.receiver = Some(receiver.into());
        self
    }

    /// Consume the builder and emit serialized metadata.
    #[must_use]
    pub fn build(self) -> CallIdentityMetadata {
        let qualified = build_qualified_name(&self.namespace, &self.simple, self.kind);
        CallIdentityMetadata {
            qualified,
            simple: self.simple,
            namespace: self.namespace,
            method_kind: self.kind,
            receiver: self.receiver,
        }
    }
}

fn build_qualified_name(namespace: &[String], simple: &str, kind: CallIdentityKind) -> String {
    if namespace.is_empty() {
        return simple.to_string();
    }

    let mut qualified = namespace.join("::");
    qualified.push(method_separator(kind));
    qualified.push_str(simple);
    qualified
}

fn method_separator(kind: CallIdentityKind) -> char {
    match kind {
        CallIdentityKind::Instance => '#',
        CallIdentityKind::Singleton | CallIdentityKind::SingletonClass => '.',
    }
}

#[cfg(test)]
mod tests {
    use super::{CallIdentityBuilder, CallIdentityKind, build_qualified_name};

    #[test]
    fn qualified_name_for_instance_method() {
        let namespace = vec![
            "Admin".to_string(),
            "Users".to_string(),
            "Controller".to_string(),
        ];
        let qualified = build_qualified_name(&namespace, "show", CallIdentityKind::Instance);
        assert_eq!(qualified, "Admin::Users::Controller#show");
    }

    #[test]
    fn qualified_name_for_singleton_method() {
        let namespace = vec!["User".to_string()];
        let qualified =
            build_qualified_name(&namespace, "authenticate", CallIdentityKind::Singleton);
        assert_eq!(qualified, "User.authenticate");
    }

    #[test]
    fn builder_emits_metadata() {
        let metadata = CallIdentityBuilder::new("find", CallIdentityKind::Singleton)
            .with_namespace(["User"])
            .with_receiver("self")
            .build();

        assert_eq!(metadata.qualified, "User.find");
        assert_eq!(metadata.simple, "find");
        assert_eq!(metadata.namespace, vec!["User".to_string()]);
        assert_eq!(metadata.method_kind, CallIdentityKind::Singleton);
        assert_eq!(metadata.receiver.as_deref(), Some("self"));
    }

    #[test]
    fn qualified_name_for_global_scope() {
        let qualified = build_qualified_name(&[], "main", CallIdentityKind::Instance);
        assert_eq!(qualified, "main");
    }
}
