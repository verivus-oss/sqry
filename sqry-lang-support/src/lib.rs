//! Shared helpers for language plugins.
//!
//! This crate consolidates utilities that were previously hand-rolled in
//! individual language crates (e.g. `sqry-lang-java`, `sqry-lang-go`).  Moving
//! the helpers here keeps relation extraction logic consistent across plugins
//! and makes future rollouts cheaper.

pub mod type_extraction;

pub mod relations {
    use sqry_core::graph::EdgeMetadata;
    use tree_sitter::Node;

    /// Build a qualified name by stitching together an optional package and a
    /// stack of enclosing scopes (classes, modules, namespaces).
    ///
    /// Empty segments are ignored to avoid spurious separators. The resulting
    /// qualified name always uses `.` as the separator so downstream lookups
    /// can rely on a consistent representation (other notations like `::` are
    /// handled by the caller before invocation).
    pub fn build_qualified_name<S, I>(package: Option<&str>, scope: I, name: &str) -> String
    where
        S: AsRef<str>,
        I: IntoIterator<Item = S>,
    {
        let mut segments = Vec::new();

        if let Some(pkg) = package.map(str::trim)
            && !pkg.is_empty()
        {
            segments.push(pkg.trim_end_matches('.').to_string());
        }

        for item in scope {
            let segment = item.as_ref().trim();
            if !segment.is_empty() {
                segments.push(segment.to_string());
            }
        }

        if !name.trim().is_empty() {
            segments.push(name.trim().to_string());
        }

        join_segments_with_separator(segments, ".").unwrap_or_default()
    }

    /// Collapse and canonicalise a nominal type string so that relation queries
    /// (`returns:` in particular) can perform reliable substring matching.
    ///
    /// The implementation mirrors the canonicaliser that lived inside the Java
    /// plugin and is general enough for other C-like languages.
    pub fn normalize_type_signature(raw: &str) -> String {
        let mut normalized = collapse_whitespace(raw);

        normalized = normalized
            .replace("< ", "<")
            .replace(" <", "<")
            .replace(" >", ">")
            .replace("> ", ">")
            .replace(", ", ",")
            .replace(" ,", ",")
            .replace(" .", ".")
            .replace(". ", ".")
            .replace(" [", "[")
            .replace("[ ", "[")
            .replace(" ]", "]")
            .replace(") ", ")")
            .replace(" )", ")")
            .replace("( ", "(");

        normalized = normalized
            .split(',')
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join(",");

        normalized.trim().to_string()
    }

    /// Determine whether an identifier should be treated as exported according
    /// to the "starts with uppercase" convention used by Go (and several other
    /// languages that share the heuristic).
    pub fn is_uppercase_export(name: &str) -> bool {
        name.chars().next().is_some_and(char::is_uppercase)
    }

    /// Join scoped segments with a custom separator (e.g., `::` for C++) after
    /// trimming whitespace and discarding empty parts. Returns `None` when every
    /// segment is empty.
    pub fn join_segments_with_separator<S, I>(segments: I, separator: &str) -> Option<String>
    where
        S: AsRef<str>,
        I: IntoIterator<Item = S>,
    {
        let filtered: Vec<String> = segments
            .into_iter()
            .map(|segment| segment.as_ref().trim().to_string())
            .filter(|segment| !segment.is_empty())
            .collect();

        if filtered.is_empty() {
            None
        } else {
            Some(filtered.join(separator))
        }
    }

    /// Track and normalise package segments (`com.example.service`) while
    /// providing memoised access to a canonical dotted form.
    #[derive(Debug, Clone, Default)]
    pub struct PackageContext {
        segments: Vec<String>,
        cached: Option<String>,
    }

    impl PackageContext {
        /// Create an empty package context.
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }

        /// Initialise the context from a dotted package string.
        pub fn from_string(value: impl AsRef<str>) -> Self {
            let mut context = Self::new();
            context.set(value);
            context
        }

        /// Replace the current package with a dotted string.
        pub fn set(&mut self, value: impl AsRef<str>) {
            self.segments = value
                .as_ref()
                .split('.')
                .map(str::trim)
                .filter(|segment| !segment.is_empty())
                .map(std::string::ToString::to_string)
                .collect();
            self.refresh_cache();
        }

        /// Clear the package entirely.
        pub fn clear(&mut self) {
            self.segments.clear();
            self.cached = None;
        }

        /// Append an additional segment to the package (e.g., when parsing
        /// `package com.example.service;` node-by-node).
        pub fn push_segment(&mut self, segment: impl AsRef<str>) {
            let trimmed = segment.as_ref().trim();
            if trimmed.is_empty() {
                return;
            }
            self.segments.push(trimmed.to_string());
            self.refresh_cache();
        }

        /// Pop the last segment off the package path, returning it if present.
        pub fn pop_segment(&mut self) -> Option<String> {
            let popped = self.segments.pop();
            self.refresh_cache();
            popped
        }

        /// Access the canonical dotted representation (`com.example.service`).
        #[must_use]
        pub fn as_str(&self) -> Option<&str> {
            self.cached.as_deref()
        }

        /// Whether the package context is empty.
        #[must_use]
        pub fn is_empty(&self) -> bool {
            self.segments.is_empty()
        }

        /// Return the segments as an immutable slice.
        #[must_use]
        pub fn segments(&self) -> &[String] {
            &self.segments
        }

        /// Join segments using a custom separator (`::` for C++).
        #[must_use]
        pub fn join_with(&self, separator: &str) -> Option<String> {
            if self.segments.is_empty() {
                None
            } else {
                Some(self.segments.join(separator))
            }
        }

        fn refresh_cache(&mut self) {
            if self.segments.is_empty() {
                self.cached = None;
            } else {
                self.cached = Some(self.segments.join("."));
            }
        }
    }

    /// Immutable view of the package + scope stack at a given point during
    /// traversal. Useful for restoring state after exploring nested nodes.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct ScopeSnapshot {
        package: Option<String>,
        scopes: Vec<String>,
    }

    impl ScopeSnapshot {
        #[must_use]
        pub fn package(&self) -> Option<&str> {
            self.package.as_deref()
        }

        #[must_use]
        pub fn scopes(&self) -> &[String] {
            &self.scopes
        }
    }

    /// Builder for nested scopes encountered while walking an AST. Maintains
    /// the active package and stack of enclosing types/modules.
    #[derive(Debug, Clone, Default)]
    pub struct ScopeTracker {
        package: PackageContext,
        scopes: Vec<String>,
    }

    impl ScopeTracker {
        /// Create an empty tracker.
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }

        /// Create a tracker primed with a known package (dotted form).
        pub fn with_package(package: impl AsRef<str>) -> Self {
            let mut tracker = Self::new();
            tracker.set_package(package);
            tracker
        }

        /// Attach an explicit package string (replacing existing segments).
        pub fn set_package(&mut self, package: impl AsRef<str>) {
            self.package.set(package);
        }

        /// Clear any tracked package information.
        pub fn clear_package(&mut self) {
            self.package.clear();
        }

        /// Borrow the current package context.
        #[must_use]
        pub fn package_context(&self) -> &PackageContext {
            &self.package
        }

        /// Mutably borrow the current package context (for incremental updates).
        pub fn package_context_mut(&mut self) -> &mut PackageContext {
            &mut self.package
        }

        /// Get the canonical dotted package string (if any).
        #[must_use]
        pub fn package(&self) -> Option<&str> {
            self.package.as_str()
        }

        /// Push a new scope segment (class, module, namespace).
        pub fn push_scope(&mut self, scope: impl AsRef<str>) {
            let trimmed = scope.as_ref().trim();
            if trimmed.is_empty() {
                return;
            }
            self.scopes.push(trimmed.to_string());
        }

        /// Pop the last scope segment, if present.
        pub fn pop_scope(&mut self) -> Option<String> {
            self.scopes.pop()
        }

        /// Clear all tracked scopes.
        pub fn clear_scopes(&mut self) {
            self.scopes.clear();
        }

        /// Borrow the scope stack.
        #[must_use]
        pub fn scopes(&self) -> &[String] {
            &self.scopes
        }

        /// Build a qualified name by stitching together the current package,
        /// scope stack, and a trailing identifier.
        #[must_use]
        pub fn qualified_name(&self, name: &str) -> String {
            build_qualified_name(self.package(), &self.scopes, name)
        }

        /// Take a snapshot of the current package and scope stack.
        pub fn snapshot(&self) -> ScopeSnapshot {
            ScopeSnapshot {
                package: self.package.as_str().map(std::string::ToString::to_string),
                scopes: self.scopes.clone(),
            }
        }

        /// Restore the tracker to a previously captured snapshot.
        pub fn restore(&mut self, snapshot: &ScopeSnapshot) {
            match snapshot.package.as_deref() {
                Some(pkg) => self.package.set(pkg),
                None => self.package.clear(),
            }
            self.scopes.clone_from(&snapshot.scopes);
        }

        /// Construct a tracker from a snapshot (helper for nested traversals).
        #[must_use]
        pub fn from_snapshot(snapshot: &ScopeSnapshot) -> Self {
            let mut tracker = Self::new();
            tracker.restore(snapshot);
            tracker
        }
    }

    /// Collapse whitespace for a node's textual content and return `None` when
    /// the result is empty. Useful for tree-sitter captures containing mixed
    /// trivia (comments, newlines, indentation).
    #[must_use]
    pub fn collapse_node_text(node: Node<'_>, content: &[u8]) -> Option<String> {
        let text = node.utf8_text(content).ok()?;
        let collapsed = collapse_whitespace(text);
        if collapsed.is_empty() {
            None
        } else {
            Some(collapsed)
        }
    }

    fn collapse_whitespace(input: &str) -> String {
        let mut out = String::with_capacity(input.len());
        let mut last_was_space = false;

        for ch in input.chars() {
            if ch.is_whitespace() {
                if !last_was_space {
                    out.push(' ');
                }
                last_was_space = true;
            } else {
                out.push(ch);
                last_was_space = false;
            }
        }

        out.trim().to_string()
    }

    /// Convenience helper to normalise raw type snippets pulled from a source
    /// buffer. Accepts UTF-8 text and delegates to [`normalize_type_signature`].
    #[must_use]
    pub fn normalize_type_node(text: &str) -> String {
        normalize_type_signature(text)
    }

    pub mod metadata {
        use super::EdgeMetadata;
        use sqry_core::graph::DetectionMethod;
        use sqry_core::relations::CallIdentityMetadata;

        /// Builder for call edge metadata that keeps the fluent API ergonomic
        /// while ensuring graph-native fields are populated consistently.
        #[derive(Debug, Default)]
        pub struct CallMetadataBuilder {
            metadata: EdgeMetadata,
        }

        impl CallMetadataBuilder {
            /// Start building call metadata.
            #[must_use]
            pub fn new() -> Self {
                Self {
                    metadata: EdgeMetadata::default(),
                }
            }

            /// Attach caller identity metadata.
            #[must_use]
            pub fn caller_identity(mut self, identity: CallIdentityMetadata) -> Self {
                self.metadata.caller_identity = Some(identity);
                self
            }

            /// Attach callee identity metadata.
            #[must_use]
            pub fn callee_identity(mut self, identity: CallIdentityMetadata) -> Self {
                self.metadata.callee_identity = Some(identity);
                self
            }

            /// Attach a human-readable reason for edge detection.
            #[must_use]
            pub fn reason(mut self, reason: impl Into<String>) -> Self {
                self.metadata.reason = Some(reason.into());
                self
            }

            /// Set the detection method for the edge.
            #[must_use]
            pub fn detection_method(mut self, method: DetectionMethod) -> Self {
                self.metadata.detection_method = method;
                self
            }

            /// Attach a confidence score for the call site.
            #[must_use]
            pub fn confidence(mut self, value: f32) -> Self {
                self.metadata.confidence = value;
                self
            }

            /// Finish building and return the metadata instance.
            #[must_use]
            pub fn build(self) -> EdgeMetadata {
                self.metadata
            }

            /// Finish building and return `None` when no metadata fields were set.
            #[must_use]
            pub fn build_option(self) -> Option<EdgeMetadata> {
                metadata_or_none(self.build())
            }
        }

        /// Builder for import edge metadata.
        #[derive(Debug, Default)]
        pub struct ImportMetadataBuilder {
            metadata: EdgeMetadata,
        }

        impl ImportMetadataBuilder {
            #[must_use]
            pub fn new() -> Self {
                Self {
                    metadata: EdgeMetadata::default(),
                }
            }

            #[must_use]
            pub fn reason(mut self, reason: impl Into<String>) -> Self {
                self.metadata.reason = Some(reason.into());
                self
            }

            #[must_use]
            pub fn detection_method(mut self, method: DetectionMethod) -> Self {
                self.metadata.detection_method = method;
                self
            }

            #[must_use]
            pub fn confidence(mut self, value: f32) -> Self {
                self.metadata.confidence = value;
                self
            }

            #[must_use]
            pub fn build(self) -> EdgeMetadata {
                self.metadata
            }

            #[must_use]
            pub fn build_option(self) -> Option<EdgeMetadata> {
                metadata_or_none(self.build())
            }
        }

        /// Builder for export edge metadata.
        #[derive(Debug, Default)]
        pub struct ExportMetadataBuilder {
            metadata: EdgeMetadata,
        }

        impl ExportMetadataBuilder {
            #[must_use]
            pub fn new() -> Self {
                Self {
                    metadata: EdgeMetadata::default(),
                }
            }

            #[must_use]
            pub fn reason(mut self, reason: impl Into<String>) -> Self {
                self.metadata.reason = Some(reason.into());
                self
            }

            #[must_use]
            pub fn detection_method(mut self, method: DetectionMethod) -> Self {
                self.metadata.detection_method = method;
                self
            }

            #[must_use]
            pub fn confidence(mut self, value: f32) -> Self {
                self.metadata.confidence = value;
                self
            }

            #[must_use]
            pub fn build(self) -> EdgeMetadata {
                self.metadata
            }

            #[must_use]
            pub fn build_option(self) -> Option<EdgeMetadata> {
                metadata_or_none(self.build())
            }
        }

        /// Convenience helper to build edge metadata using a closure, returning
        /// `None` when the closure leaves the metadata empty.
        pub fn build_edge_metadata<F>(build: F) -> Option<EdgeMetadata>
        where
            F: FnOnce(EdgeMetadata) -> EdgeMetadata,
        {
            metadata_or_none(build(EdgeMetadata::default()))
        }

        fn metadata_or_none(metadata: EdgeMetadata) -> Option<EdgeMetadata> {
            if is_default_metadata(&metadata) {
                None
            } else {
                Some(metadata)
            }
        }

        fn is_default_metadata(metadata: &EdgeMetadata) -> bool {
            metadata.span.is_none()
                && (metadata.confidence - 1.0).abs() < f32::EPSILON
                && matches!(metadata.detection_method, DetectionMethod::Unknown)
                && metadata.reason.is_none()
                && metadata.caller_identity.is_none()
                && metadata.callee_identity.is_none()
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use sqry_core::relations::{CallIdentityBuilder, CallIdentityKind};

            #[test]
            fn builder_sets_identity_and_reason() {
                let caller = CallIdentityBuilder::new("handle", CallIdentityKind::Instance)
                    .with_namespace(["Controller"])
                    .build();
                let callee = CallIdentityBuilder::new("find", CallIdentityKind::Instance)
                    .with_namespace(["Service"])
                    .build();
                let meta = CallMetadataBuilder::new()
                    .caller_identity(caller.clone())
                    .callee_identity(callee.clone())
                    .reason("resolved via AST")
                    .build();
                assert_eq!(meta.caller_identity, Some(caller));
                assert_eq!(meta.callee_identity, Some(callee));
                assert_eq!(meta.reason.as_deref(), Some("resolved via AST"));
            }

            #[test]
            fn builder_sets_confidence_and_detection_method() {
                let meta = CallMetadataBuilder::new()
                    .confidence(0.6)
                    .detection_method(DetectionMethod::Heuristic)
                    .build();
                assert!((meta.confidence - 0.6).abs() < f32::EPSILON);
                assert_eq!(meta.detection_method, DetectionMethod::Heuristic);
            }

            #[test]
            fn import_builder_sets_fields() {
                let meta = ImportMetadataBuilder::new()
                    .reason("stdlib import")
                    .detection_method(DetectionMethod::ASTAnalysis)
                    .confidence(0.9)
                    .build();
                assert_eq!(meta.reason.as_deref(), Some("stdlib import"));
                assert_eq!(meta.detection_method, DetectionMethod::ASTAnalysis);
                assert!((meta.confidence - 0.9).abs() < f32::EPSILON);
            }

            #[test]
            fn export_builder_sets_fields() {
                let meta = ExportMetadataBuilder::new()
                    .reason("public export")
                    .detection_method(DetectionMethod::Manual)
                    .confidence(1.0)
                    .build();
                assert_eq!(meta.reason.as_deref(), Some("public export"));
                assert_eq!(meta.detection_method, DetectionMethod::Manual);
                assert!((meta.confidence - 1.0).abs() < f32::EPSILON);
            }

            #[test]
            fn build_option_skips_empty_metadata() {
                assert!(CallMetadataBuilder::new().build_option().is_none());

                let maybe = CallMetadataBuilder::new()
                    .reason("constructor")
                    .build_option();
                assert!(maybe.is_some());

                let import = ImportMetadataBuilder::new()
                    .reason("wildcard import")
                    .build_option();
                assert!(import.is_some());

                let export = ExportMetadataBuilder::new()
                    .reason("public export")
                    .build_option();
                assert!(export.is_some());
            }

            #[test]
            fn build_edge_metadata_helper_runs_closure() {
                let meta = build_edge_metadata(|mut meta| {
                    meta.reason = Some("test".to_string());
                    meta
                });
                assert!(meta.is_some());

                assert!(build_edge_metadata(|meta| meta).is_none());
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use tree_sitter::Parser;

        #[test]
        fn build_qualified_name_handles_package_and_scope() {
            let scope = vec!["outer".to_string(), "Inner".to_string()];
            let fq = build_qualified_name(Some("com.example"), &scope, "method");
            assert_eq!(fq, "com.example.outer.Inner.method");
        }

        #[test]
        fn build_qualified_name_skips_empty_segments() {
            let scope = vec![String::new(), "Service".to_string()];
            let fq = build_qualified_name(Some(""), &scope, "run");
            assert_eq!(fq, "Service.run");
        }

        #[test]
        fn normalize_type_signature_canonicalises_generics() {
            let raw = "java.util.Optional < List < String > >";
            assert_eq!(
                normalize_type_signature(raw),
                "java.util.Optional<List<String>>"
            );
        }

        #[test]
        fn normalize_type_signature_handles_arrays() {
            let raw = "Result [] ";
            assert_eq!(normalize_type_signature(raw), "Result[]");
        }

        #[test]
        fn package_context_tracks_segments() {
            let mut ctx = PackageContext::new();
            ctx.push_segment("com");
            ctx.push_segment(" example ");
            ctx.push_segment("service");
            assert_eq!(ctx.as_str(), Some("com.example.service"));

            ctx.pop_segment();
            assert_eq!(ctx.as_str(), Some("com.example"));

            ctx.set("  org.example.models  ");
            assert_eq!(
                ctx.segments(),
                &[
                    "org".to_string(),
                    "example".to_string(),
                    "models".to_string()
                ]
            );
            assert_eq!(
                ctx.join_with("::"),
                Some("org::example::models".to_string())
            );
        }

        #[test]
        fn scope_tracker_builds_qualified_name_and_restores() {
            let mut tracker = ScopeTracker::with_package("com.example");
            tracker.push_scope("outer");
            tracker.push_scope("Inner");
            assert_eq!(tracker.qualified_name("run"), "com.example.outer.Inner.run");

            let snapshot = tracker.snapshot();
            tracker.push_scope("Nested");
            assert_eq!(
                tracker.scopes(),
                &[
                    "outer".to_string(),
                    "Inner".to_string(),
                    "Nested".to_string()
                ]
            );

            tracker.restore(&snapshot);
            assert_eq!(
                tracker.scopes(),
                &["outer".to_string(), "Inner".to_string()]
            );
            assert_eq!(tracker.package(), Some("com.example"));

            let restored = ScopeTracker::from_snapshot(&snapshot);
            assert_eq!(restored.scopes(), tracker.scopes());
            assert_eq!(restored.package(), tracker.package());
        }

        #[test]
        fn collapse_node_text_trims_and_collapses() {
            let mut parser = Parser::new();
            parser
                .set_language(&tree_sitter_javascript::LANGUAGE.into())
                .expect("language");

            let source = "function test() {\n  return foo   (\n    bar);\n}\n";
            let tree = parser.parse(source, None).expect("parse tree");
            let root = tree.root_node();
            let function = root.named_child(0).expect("function declaration");
            let body = function.child_by_field_name("body").expect("function body");
            let return_stmt = body.named_child(0).expect("return statement");
            let collapsed = collapse_node_text(return_stmt, source.as_bytes()).expect("collapsed");

            assert_eq!(collapsed, "return foo ( bar);");
        }

        #[test]
        fn is_uppercase_export_follows_go_convention() {
            assert!(is_uppercase_export("HTTPServer"));
            assert!(is_uppercase_export("Serve"));
            assert!(!is_uppercase_export("serve"));
            assert!(!is_uppercase_export("_private"));
        }

        #[test]
        fn join_segments_with_separator_trims_and_filters() {
            let segments = vec![" namespace ", "", "Class", "method "];
            let joined = join_segments_with_separator(segments, "::");
            assert_eq!(joined, Some("namespace::Class::method".to_string()));
        }

        #[test]
        fn join_segments_with_separator_returns_none_when_empty() {
            let segments = vec!["", " ", "\t"];
            assert!(join_segments_with_separator(segments, ".").is_none());
        }
    }
}
