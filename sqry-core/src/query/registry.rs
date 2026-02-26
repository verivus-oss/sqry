//! Field registry for query validation and optimization
//!
//! This module provides the field registry which maintains a catalog of all queryable fields,
//! both core fields and plugin-provided fields. The registry is used during validation to
//! check field names, operators, and value types.

use std::collections::HashMap;

use super::types::{FieldDescriptor, FieldType, Operator};

/// Registry of all queryable fields (core + plugin fields)
///
/// The field registry maintains a mapping of field names to their descriptors,
/// which include type information, supported operators, indexing status, and documentation.
///
/// # Example
///
/// ```
/// use sqry_core::query::registry::FieldRegistry;
///
/// let registry = FieldRegistry::with_core_fields();
/// assert!(registry.get("kind").is_some());
/// assert!(registry.get("name").is_some());
/// ```
#[derive(Debug, Clone)]
pub struct FieldRegistry {
    /// Map of field names to descriptors
    fields: HashMap<String, FieldDescriptor>,
    /// Map of field aliases to canonical names (e.g., "file" -> "path", "language" -> "lang")
    aliases: HashMap<String, String>,
}

impl FieldRegistry {
    /// Create an empty field registry
    #[must_use]
    pub fn new() -> Self {
        Self {
            fields: HashMap::new(),
            aliases: HashMap::new(),
        }
    }

    /// Create a field registry with core fields pre-populated
    ///
    /// Core fields are always available and include:
    /// - `kind`: Node type (function, method, class, etc.)
    /// - `name`: Node name (exact or regex match)
    /// - `path`: File path (glob or regex match) - aliased as `file`
    /// - `lang`: Programming language - aliased as `language`
    /// - `repo`: Repository filter (glob pattern)
    /// - `parent`: Parent symbol name (regex match)
    /// - `scope`: Scope type (file, module, class, function, block)
    /// - `text`: Full-text search in symbol body (NOT indexed)
    #[must_use]
    pub fn with_core_fields() -> Self {
        let mut registry = Self::new();

        for field in core_fields() {
            registry.add_field(field);
        }

        // Register field aliases for backward compatibility and convenience
        registry.add_alias("file", "path"); // file: is alias for path:
        registry.add_alias("language", "lang"); // language: is alias for lang:

        registry
    }

    /// Add a field descriptor to the registry
    ///
    /// If a field with the same name already exists, it will be replaced.
    pub fn add_field(&mut self, descriptor: FieldDescriptor) {
        self.fields.insert(descriptor.name.to_string(), descriptor);
    }

    /// Register an alias for a field
    ///
    /// Aliases allow alternative names for fields (e.g., "file" as alias for "path").
    /// The canonical field must already exist in the registry.
    ///
    /// # Arguments
    ///
    /// * `alias` - The alias name (e.g., "file")
    /// * `canonical` - The canonical field name (e.g., "path")
    ///
    /// # Panics
    ///
    /// Panics if the canonical field does not exist in the registry.
    pub fn add_alias(&mut self, alias: impl Into<String>, canonical: impl Into<String>) {
        let alias = alias.into();
        let canonical = canonical.into();

        // Verify canonical field exists
        assert!(
            self.fields.contains_key(&canonical),
            "Cannot add alias '{alias}' -> '{canonical}': canonical field '{canonical}' does not exist"
        );

        self.aliases.insert(alias, canonical);
    }

    /// Get a field descriptor by name or alias
    ///
    /// If the name is an alias, this resolves to the canonical field.
    #[inline]
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&FieldDescriptor> {
        // Try direct lookup first
        if let Some(descriptor) = self.fields.get(name) {
            return Some(descriptor);
        }

        // Try alias resolution
        if let Some(canonical) = self.aliases.get(name) {
            return self.fields.get(canonical);
        }

        None
    }

    /// Resolve a field name to its canonical form
    ///
    /// If the name is an alias, returns the canonical name.
    /// Otherwise, returns the name unchanged if it exists in the registry.
    ///
    /// Returns None if the field name (or alias) doesn't exist.
    pub fn resolve_canonical<'a>(&'a self, name: &'a str) -> Option<&'a str> {
        // If it's a direct field, return it
        if self.fields.contains_key(name) {
            return Some(name);
        }

        // If it's an alias, return the canonical name
        self.aliases.get(name).map(std::string::String::as_str)
    }

    /// Get all field names
    pub fn field_names(&self) -> Vec<&str> {
        self.fields
            .keys()
            .map(std::string::String::as_str)
            .collect()
    }

    /// Check if a field or alias exists
    #[inline]
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.fields.contains_key(name) || self.aliases.contains_key(name)
    }

    /// Get the number of fields in the registry
    #[must_use]
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    /// Check if the registry is empty
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// Add plugin fields to the registry (Phase 7+)
    ///
    /// Merges plugin-specific fields with existing fields. If a field name collides with an
    /// existing field, the plugin field will NOT be added.
    ///
    /// # Arguments
    ///
    /// * `fields` - Slice of field descriptors from a plugin
    ///
    /// # Returns
    ///
    /// Vector of field names that collided (were NOT added)
    ///
    /// # Example
    ///
    /// ```ignore
    /// let mut registry = FieldRegistry::with_core_fields();
    /// let rust_plugin = RustPlugin::new();
    /// let collisions = registry.add_plugin_fields(rust_plugin.fields());
    /// assert!(collisions.is_empty()); // No collisions with core fields
    /// ```
    #[must_use = "collision information should be logged or handled"]
    pub fn add_plugin_fields(&mut self, fields: &[FieldDescriptor]) -> Vec<String> {
        let mut collisions = Vec::new();

        for field in fields {
            if self.contains(field.name) {
                // Collision detected - skip this field
                log::debug!(
                    "Plugin field collision: '{}' already exists, skipping plugin version",
                    field.name
                );
                collisions.push(field.name.to_string());
            } else {
                // No collision - add the field
                log::debug!("Registering plugin field: '{}'", field.name);
                self.add_field(field.clone());
            }
        }

        if collisions.is_empty() {
            log::debug!(
                "Plugin field registration complete: {} fields added, no collisions",
                fields.len()
            );
        } else {
            log::debug!(
                "Plugin field registration complete: {} fields added, {} collisions ({})",
                fields.len() - collisions.len(),
                collisions.len(),
                collisions.join(", ")
            );
        }

        collisions
    }

    /// Get plugin IDs for cache key generation (Phase 7+)
    ///
    /// Returns a sorted list of unique plugin identifiers based on registered fields.
    /// This is used to create cache keys that are sensitive to which plugins are loaded.
    ///
    /// Note: Since we don't track which field came from which plugin, this method
    /// currently returns an empty vector. In the future, we could add plugin metadata
    /// to `FieldDescriptor` to enable this functionality.
    #[must_use]
    pub fn plugin_ids(&self) -> Vec<String> {
        // Phase 7: Not implemented yet - would require tracking plugin source per field
        Vec::new()
    }
}

impl Default for FieldRegistry {
    fn default() -> Self {
        Self::with_core_fields()
    }
}

/// Core field descriptors
///
/// These fields are always available regardless of which plugins are loaded.
///
/// # Core Fields
///
/// - `kind`: Node type (function, method, class, etc.) - indexed
/// - `name`: Node name - indexed
/// - `path`: File path (glob-aware) - indexed (aliased as `file`)
/// - `lang`: Programming language - indexed (aliased as `language`)
/// - `repo`: Repository filter (glob pattern) - NOT indexed
/// - `parent`: Parent symbol name (regex) - NOT indexed
/// - `scope`: Scope type - partially indexed
/// - `text`: Full-text search in symbol body - NOT indexed
/// - `scope.type`: Type of scope containing symbol (P2-34) - NOT indexed
/// - `scope.name`: Name of scope containing symbol (P2-34) - NOT indexed
/// - `scope.parent`: Name of parent scope (P2-34) - NOT indexed
/// - `scope.ancestor`: Name of ancestor scope (P2-34) - NOT indexed
#[must_use]
#[allow(clippy::too_many_lines)] // Large field registry kept together for readability and auditing.
pub fn core_fields() -> Vec<FieldDescriptor> {
    vec![
        FieldDescriptor {
            name: "kind",
            field_type: FieldType::Enum(vec![
                "function",
                "method",
                "class",
                "struct",
                "trait",
                "enum",
                "interface",
                "module",
                "variable",
                "constant",
                "type",
                "namespace",
                "property",
                "parameter",
                "import",
            ]),
            operators: &[Operator::Equal, Operator::Regex],
            indexed: true,
            doc: "Node type: function, class, method, struct, etc.",
        },
        FieldDescriptor {
            name: "name",
            field_type: FieldType::String,
            operators: &[Operator::Equal, Operator::Regex],
            indexed: true,
            doc: "Node name (exact match with ':' or regex with '~=')",
        },
        FieldDescriptor {
            name: "path",
            field_type: FieldType::Path,
            operators: &[Operator::Equal, Operator::Regex],
            indexed: true,
            doc: "File path (glob pattern with ':' or regex with '~=')",
        },
        FieldDescriptor {
            name: "lang",
            field_type: FieldType::String,
            operators: &[Operator::Equal, Operator::Regex],
            indexed: true,
            doc: "Programming language",
        },
        FieldDescriptor {
            name: "repo",
            field_type: FieldType::String,
            operators: &[Operator::Equal],
            indexed: false,
            doc: "Repository filter (glob pattern, e.g., 'repo:backend-*'). Used in workspace queries to filter results by repository name.",
        },
        FieldDescriptor {
            name: "parent",
            field_type: FieldType::String,
            operators: &[Operator::Equal, Operator::Regex],
            indexed: false,
            doc: "Parent symbol name (exact match with ':' or regex with '~='). Matches symbols with a specific parent (e.g., methods in a class).",
        },
        FieldDescriptor {
            name: "scope",
            field_type: FieldType::Enum(vec!["file", "module", "class", "function", "block"]),
            operators: &[Operator::Equal],
            indexed: false,
            doc: "Scope type: file, module, class, function, block",
        },
        FieldDescriptor {
            name: "text",
            field_type: FieldType::String,
            operators: &[Operator::Regex],
            indexed: false,
            doc: "Full-text search in symbol body (code + comments). NOT indexed - use indexed fields first for performance!",
        },
        // Sprint 2: Relation fields
        FieldDescriptor {
            name: "callers",
            field_type: FieldType::String,
            operators: &[Operator::Equal],
            indexed: true,
            doc: "Match symbols that call the specified function (requires index with relations)",
        },
        FieldDescriptor {
            name: "callees",
            field_type: FieldType::String,
            operators: &[Operator::Equal],
            indexed: true,
            doc: "Match symbols called by the specified function (requires index with relations)",
        },
        FieldDescriptor {
            name: "imports",
            field_type: FieldType::String,
            operators: &[Operator::Equal],
            indexed: true,
            doc: "Match files that import the specified module (requires index with relations)",
        },
        FieldDescriptor {
            name: "exports",
            field_type: FieldType::String,
            operators: &[Operator::Equal],
            indexed: true,
            doc: "Match files that export the specified symbol (requires index with relations)",
        },
        FieldDescriptor {
            name: "returns",
            field_type: FieldType::String,
            operators: &[Operator::Equal, Operator::Regex],
            indexed: true,
            doc: "Match functions with the specified return type (requires index with type metadata)",
        },
        // CD Static Analysis: Trait implementation predicates
        FieldDescriptor {
            name: "impl",
            field_type: FieldType::String,
            operators: &[Operator::Equal],
            indexed: true,
            doc: "Match types implementing a trait (e.g., impl:Debug finds types that implement Debug)",
        },
        // P2-33: Cross-file reference predicates
        FieldDescriptor {
            name: "references",
            field_type: FieldType::String,
            operators: &[Operator::Equal],
            indexed: true,
            doc: "Match symbols that have cross-file references (requires graph reference edges). Example: references:connect finds symbols named 'connect' that are referenced elsewhere.",
        },
        // P2-34 Phase 2: Scope filtering fields
        FieldDescriptor {
            name: "scope.type",
            field_type: FieldType::Enum(vec![
                "module",
                "function",
                "class",
                "struct",
                "method",
                "block",
                "namespace",
                "trait",
                "impl",
                "interface",
                "enum",
            ]),
            operators: &[Operator::Equal, Operator::Regex],
            indexed: false,
            doc: "Type of the scope containing this symbol (e.g., 'function', 'class', 'module'). Evaluated via sequential scan over file_scopes + scope_id.",
        },
        FieldDescriptor {
            name: "scope.name",
            field_type: FieldType::String,
            operators: &[Operator::Equal, Operator::Regex],
            indexed: false,
            doc: "Name of the scope containing this symbol (e.g., 'UserService', 'connect'). Evaluated via sequential scan over file_scopes + scope_id.",
        },
        FieldDescriptor {
            name: "scope.parent",
            field_type: FieldType::String,
            operators: &[Operator::Equal, Operator::Regex],
            indexed: false,
            doc: "Name of the parent scope (immediate containing scope). Evaluated via sequential scan over file_scopes + scope_id.",
        },
        FieldDescriptor {
            name: "scope.ancestor",
            field_type: FieldType::String,
            operators: &[Operator::Equal, Operator::Regex],
            indexed: false,
            doc: "Name of any ancestor scope (walks up the scope hierarchy). Evaluated via sequential scan over file_scopes + scope_id.",
        },
        // CD Static Analysis: Cross-file discovery predicates
        FieldDescriptor {
            name: "duplicates",
            field_type: FieldType::Enum(vec!["body", "function", "signature", "struct"]),
            operators: &[Operator::Equal],
            indexed: false,
            doc: "Find duplicate code: duplicates:body (function bodies), duplicates:signature (return types), duplicates:struct (struct layouts)",
        },
        FieldDescriptor {
            name: "unused",
            field_type: FieldType::Enum(vec!["public", "private", "function", "struct", "all"]),
            operators: &[Operator::Equal],
            indexed: false,
            doc: "Find unused symbols: unused:public, unused:private, unused:function, unused:struct, or unused:all",
        },
        FieldDescriptor {
            name: "circular",
            field_type: FieldType::Enum(vec!["calls", "imports", "all"]),
            operators: &[Operator::Equal],
            indexed: false,
            doc: "Find circular dependencies: circular:calls (mutual recursion), circular:imports (import cycles), circular:all",
        },
        // P2-XX: Boolean metadata fields (from NodeEntry)
        FieldDescriptor {
            name: "async",
            field_type: FieldType::Bool,
            operators: &[Operator::Equal],
            indexed: false,
            doc: "Whether the function/method is async: async:true or async:false",
        },
        FieldDescriptor {
            name: "static",
            field_type: FieldType::Bool,
            operators: &[Operator::Equal],
            indexed: false,
            doc: "Whether the member is static: static:true or static:false",
        },
        FieldDescriptor {
            name: "visibility",
            field_type: FieldType::String,
            operators: &[Operator::Equal],
            indexed: false,
            doc: "Visibility modifier: visibility:pub, visibility:private, etc.",
        },
        FieldDescriptor {
            name: "returns",
            field_type: FieldType::String,
            operators: &[Operator::Equal],
            indexed: false,
            doc: "Return type filter: returns:bool, returns:Optional<User>, etc. Uses substring matching.",
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_registry_is_empty() {
        let registry = FieldRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn test_with_core_fields() {
        let registry = FieldRegistry::with_core_fields();
        assert!(!registry.is_empty());
        assert_eq!(registry.len(), 25); // kind, name, path, lang, scope, text, callers, callees, imports, exports, returns, impl, repo, parent, references (P2-33) + scope.type, scope.name, scope.parent, scope.ancestor (P2-34) + duplicates, unused, circular (CD predicates)
    }

    #[test]
    fn test_default_has_core_fields() {
        let registry = FieldRegistry::default();
        assert_eq!(registry.len(), 25); // Updated for Sprint 2 relation fields + P2-33 references + P2-34 scope.* fields + CD Static Analysis predicates
    }

    #[test]
    fn test_core_field_kind_exists() {
        let registry = FieldRegistry::with_core_fields();
        let kind_field = registry.get("kind");
        assert!(kind_field.is_some());

        let kind = kind_field.unwrap();
        assert_eq!(kind.name, "kind");
        assert!(kind.indexed);
        assert!(kind.supports_operator(&Operator::Equal));
        assert!(kind.supports_operator(&Operator::Regex));
        assert!(!kind.supports_operator(&Operator::Greater));
    }

    #[test]
    fn test_core_field_name_exists() {
        let registry = FieldRegistry::with_core_fields();
        let name_field = registry.get("name");
        assert!(name_field.is_some());

        let name = name_field.unwrap();
        assert_eq!(name.name, "name");
        assert!(name.indexed);
        assert!(matches!(name.field_type, FieldType::String));
    }

    #[test]
    fn test_core_field_path_exists() {
        let registry = FieldRegistry::with_core_fields();
        let path_field = registry.get("path");
        assert!(path_field.is_some());

        let path = path_field.unwrap();
        assert_eq!(path.name, "path");
        assert!(path.indexed);
        assert!(matches!(path.field_type, FieldType::Path));
    }

    #[test]
    fn test_core_field_lang_exists() {
        let registry = FieldRegistry::with_core_fields();
        let lang_field = registry.get("lang");
        assert!(lang_field.is_some());

        let lang = lang_field.unwrap();
        assert_eq!(lang.name, "lang");
        assert!(lang.indexed);
        assert!(matches!(lang.field_type, FieldType::String));
        assert!(lang.supports_operator(&Operator::Equal));
        assert!(lang.supports_operator(&Operator::Regex));
    }

    #[test]
    fn test_core_field_scope_exists() {
        let registry = FieldRegistry::with_core_fields();
        let scope_field = registry.get("scope");
        assert!(scope_field.is_some());

        let scope = scope_field.unwrap();
        assert_eq!(scope.name, "scope");
        assert!(!scope.indexed); // Scope is NOT indexed
    }

    #[test]
    fn test_core_field_text_exists() {
        let registry = FieldRegistry::with_core_fields();
        let text_field = registry.get("text");
        assert!(text_field.is_some());

        let text = text_field.unwrap();
        assert_eq!(text.name, "text");
        assert!(!text.indexed); // Text is NOT indexed
        assert!(text.supports_operator(&Operator::Regex));
        assert!(!text.supports_operator(&Operator::Equal));
    }

    #[test]
    fn test_get_nonexistent_field() {
        let registry = FieldRegistry::with_core_fields();
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn test_add_custom_field() {
        let mut registry = FieldRegistry::new();

        let custom_field = FieldDescriptor {
            name: "async",
            field_type: FieldType::Bool,
            operators: &[Operator::Equal],
            indexed: false,
            doc: "Whether function is async",
        };

        registry.add_field(custom_field);

        assert_eq!(registry.len(), 1);
        assert!(registry.contains("async"));

        let async_field = registry.get("async").unwrap();
        assert_eq!(async_field.name, "async");
        assert!(matches!(async_field.field_type, FieldType::Bool));
    }

    #[test]
    fn test_add_field_replaces_existing() {
        let mut registry = FieldRegistry::with_core_fields();
        let original_len = registry.len();

        // Replace "kind" field with custom version
        let custom_kind = FieldDescriptor {
            name: "kind",
            field_type: FieldType::String,
            operators: &[Operator::Equal],
            indexed: false,
            doc: "Custom kind field",
        };

        registry.add_field(custom_kind);

        // Should have same number of fields (replaced, not added)
        assert_eq!(registry.len(), original_len);

        let kind_field = registry.get("kind").unwrap();
        assert!(matches!(kind_field.field_type, FieldType::String));
        assert!(!kind_field.indexed);
    }

    #[test]
    fn test_field_names() {
        let registry = FieldRegistry::with_core_fields();
        let names = registry.field_names();

        assert_eq!(names.len(), 25); // Updated for Sprint 2 + P2-33 references + P2-34 scope.* + CD predicates (impl, duplicates, unused, circular)
        assert!(names.contains(&"kind"));
        assert!(names.contains(&"name"));
        assert!(names.contains(&"path"));
        assert!(names.contains(&"lang"));
        assert!(names.contains(&"scope"));
        assert!(names.contains(&"text"));
        assert!(names.contains(&"callers"));
        assert!(names.contains(&"callees"));
        assert!(names.contains(&"imports"));
        assert!(names.contains(&"exports"));
        assert!(names.contains(&"returns"));
        assert!(names.contains(&"references"));
        assert!(names.contains(&"repo"));
        assert!(names.contains(&"parent"));
        assert!(names.contains(&"scope.type")); // P2-34
        assert!(names.contains(&"scope.name")); // P2-34
        assert!(names.contains(&"scope.parent")); // P2-34
        assert!(names.contains(&"scope.ancestor")); // P2-34
    }

    #[test]
    fn test_contains() {
        let registry = FieldRegistry::with_core_fields();

        assert!(registry.contains("kind"));
        assert!(registry.contains("name"));
        // async, static, visibility are now core fields (added for NodeEntry metadata)
        assert!(registry.contains("async"));
        assert!(registry.contains("static"));
        assert!(registry.contains("visibility"));
        assert!(!registry.contains("nonexistent_field"));
    }

    #[test]
    fn test_kind_enum_values() {
        let registry = FieldRegistry::with_core_fields();
        let kind_field = registry.get("kind").unwrap();

        if let FieldType::Enum(values) = &kind_field.field_type {
            assert!(values.contains(&"function"));
            assert!(values.contains(&"method"));
            assert!(values.contains(&"class"));
            assert!(values.contains(&"struct"));
            assert!(values.contains(&"trait"));
            assert!(values.contains(&"enum"));
            assert!(values.contains(&"interface"));
            assert!(values.contains(&"module"));
        } else {
            panic!("Expected Enum field type for 'kind'");
        }
    }

    #[test]
    fn test_lang_enum_values() {
        let registry = FieldRegistry::with_core_fields();
        let lang_field = registry.get("lang").unwrap();

        assert!(matches!(lang_field.field_type, FieldType::String));
        assert!(lang_field.supports_operator(&Operator::Equal));
        assert!(lang_field.supports_operator(&Operator::Regex));
    }

    #[test]
    fn test_scope_enum_values() {
        let registry = FieldRegistry::with_core_fields();
        let scope_field = registry.get("scope").unwrap();

        if let FieldType::Enum(values) = &scope_field.field_type {
            assert!(values.contains(&"file"));
            assert!(values.contains(&"module"));
            assert!(values.contains(&"class"));
            assert!(values.contains(&"function"));
            assert!(values.contains(&"block"));
            assert_eq!(values.len(), 5);
        } else {
            panic!("Expected Enum field type for 'scope'");
        }
    }

    // Task 7: Plugin Integration Tests

    #[test]
    fn test_add_plugin_fields_no_collision() {
        let mut registry = FieldRegistry::with_core_fields();
        let original_len = registry.len();

        // Use unique plugin field names that don't collide with core fields
        let plugin_fields = vec![
            FieldDescriptor {
                name: "plugin_async",
                field_type: FieldType::Bool,
                operators: &[Operator::Equal],
                indexed: false,
                doc: "Plugin-specific async field",
            },
            FieldDescriptor {
                name: "plugin_visibility",
                field_type: FieldType::String,
                operators: &[Operator::Equal],
                indexed: false,
                doc: "Plugin-specific visibility field",
            },
        ];

        let collisions = registry.add_plugin_fields(&plugin_fields);

        // No collisions expected
        assert_eq!(collisions.len(), 0);

        // Fields should be added
        assert_eq!(registry.len(), original_len + 2);
        assert!(registry.contains("plugin_async"));
        assert!(registry.contains("plugin_visibility"));
    }

    #[test]
    fn test_add_plugin_fields_with_collision() {
        let mut registry = FieldRegistry::with_core_fields();
        let original_len = registry.len();

        let plugin_fields = vec![
            FieldDescriptor {
                name: "kind", // Collides with core field
                field_type: FieldType::String,
                operators: &[Operator::Equal],
                indexed: false,
                doc: "Custom kind field",
            },
            FieldDescriptor {
                name: "plugin_unique",
                field_type: FieldType::Bool,
                operators: &[Operator::Equal],
                indexed: false,
                doc: "Unique plugin field",
            },
        ];

        let collisions = registry.add_plugin_fields(&plugin_fields);

        // Should have 1 collision (kind)
        assert_eq!(collisions.len(), 1);
        assert_eq!(collisions[0], "kind");

        // Only plugin_unique should be added
        assert_eq!(registry.len(), original_len + 1);
        assert!(registry.contains("plugin_unique"));

        // kind should still be the original core field
        let kind_field = registry.get("kind").unwrap();
        assert!(matches!(kind_field.field_type, FieldType::Enum(_)));
        assert!(kind_field.indexed);
    }

    #[test]
    fn test_add_plugin_fields_all_collisions() {
        let mut registry = FieldRegistry::with_core_fields();
        let original_len = registry.len();

        let plugin_fields = vec![
            FieldDescriptor {
                name: "kind",
                field_type: FieldType::String,
                operators: &[Operator::Equal],
                indexed: false,
                doc: "Custom kind",
            },
            FieldDescriptor {
                name: "name",
                field_type: FieldType::String,
                operators: &[Operator::Equal],
                indexed: false,
                doc: "Custom name",
            },
        ];

        let collisions = registry.add_plugin_fields(&plugin_fields);

        // All fields should collide
        assert_eq!(collisions.len(), 2);
        assert!(collisions.contains(&"kind".to_string()));
        assert!(collisions.contains(&"name".to_string()));

        // No new fields added
        assert_eq!(registry.len(), original_len);
    }

    #[test]
    fn test_plugin_ids_placeholder() {
        let registry = FieldRegistry::with_core_fields();
        let ids = registry.plugin_ids();

        // Currently returns empty vector (Phase 7 placeholder)
        assert_eq!(ids.len(), 0);
    }

    // FR-2025-015: Tests for alias support and new predicates (repo, parent)

    #[test]
    fn test_alias_file_to_path() {
        let registry = FieldRegistry::with_core_fields();

        // "file" should resolve to "path"
        let via_alias = registry.get("file");
        let direct = registry.get("path");

        assert!(via_alias.is_some());
        assert!(direct.is_some());

        // Should return the same descriptor
        assert_eq!(via_alias.unwrap().name, direct.unwrap().name);
        assert_eq!(via_alias.unwrap().name, "path");
    }

    #[test]
    fn test_alias_language_to_lang() {
        let registry = FieldRegistry::with_core_fields();

        // "language" should resolve to "lang"
        let via_alias = registry.get("language");
        let direct = registry.get("lang");

        assert!(via_alias.is_some());
        assert!(direct.is_some());

        // Should return the same descriptor
        assert_eq!(via_alias.unwrap().name, direct.unwrap().name);
        assert_eq!(via_alias.unwrap().name, "lang");
    }

    #[test]
    fn test_contains_recognizes_aliases() {
        let registry = FieldRegistry::with_core_fields();

        // Direct fields
        assert!(registry.contains("path"));
        assert!(registry.contains("lang"));

        // Aliases
        assert!(registry.contains("file"));
        assert!(registry.contains("language"));
    }

    #[test]
    fn test_resolve_canonical_with_direct_field() {
        let registry = FieldRegistry::with_core_fields();

        assert_eq!(registry.resolve_canonical("kind"), Some("kind"));
        assert_eq!(registry.resolve_canonical("path"), Some("path"));
    }

    #[test]
    fn test_resolve_canonical_with_alias() {
        let registry = FieldRegistry::with_core_fields();

        assert_eq!(registry.resolve_canonical("file"), Some("path"));
        assert_eq!(registry.resolve_canonical("language"), Some("lang"));
    }

    #[test]
    fn test_resolve_canonical_with_nonexistent() {
        let registry = FieldRegistry::with_core_fields();

        assert_eq!(registry.resolve_canonical("nonexistent"), None);
        assert_eq!(registry.resolve_canonical("unknown_alias"), None);
    }

    #[test]
    fn test_add_alias_to_custom_field() {
        let mut registry = FieldRegistry::new();

        let custom_field = FieldDescriptor {
            name: "async",
            field_type: FieldType::Bool,
            operators: &[Operator::Equal],
            indexed: false,
            doc: "Whether function is async",
        };

        registry.add_field(custom_field);
        registry.add_alias("is_async", "async");

        // Both should work
        assert!(registry.contains("async"));
        assert!(registry.contains("is_async"));

        let via_alias = registry.get("is_async").unwrap();
        assert_eq!(via_alias.name, "async");
    }

    #[test]
    #[should_panic(expected = "canonical field 'nonexistent' does not exist")]
    fn test_add_alias_to_nonexistent_field_panics() {
        let mut registry = FieldRegistry::new();
        registry.add_alias("bad_alias", "nonexistent");
    }

    #[test]
    fn test_core_field_repo_exists() {
        let registry = FieldRegistry::with_core_fields();
        let repo_field = registry.get("repo");
        assert!(repo_field.is_some());

        let repo = repo_field.unwrap();
        assert_eq!(repo.name, "repo");
        assert!(!repo.indexed); // Repo is a filter, not indexed
        assert!(matches!(repo.field_type, FieldType::String));
        assert!(repo.supports_operator(&Operator::Equal));
        assert!(!repo.supports_operator(&Operator::Greater));
    }

    #[test]
    fn test_core_field_parent_exists() {
        let registry = FieldRegistry::with_core_fields();
        let parent_field = registry.get("parent");
        assert!(parent_field.is_some());

        let parent = parent_field.unwrap();
        assert_eq!(parent.name, "parent");
        assert!(!parent.indexed); // Parent is not indexed
        assert!(matches!(parent.field_type, FieldType::String));
        assert!(parent.supports_operator(&Operator::Equal));
        assert!(parent.supports_operator(&Operator::Regex));
    }

    #[test]
    fn test_alias_does_not_affect_field_count() {
        let registry = FieldRegistry::with_core_fields();

        // Aliases should not be counted as separate fields
        assert_eq!(registry.len(), 25); // Updated for P2-33 references + P2-34 scope.* + CD predicates

        // But both aliases and canonical names should be accessible
        assert!(registry.contains("file"));
        assert!(registry.contains("path"));
        assert!(registry.contains("language"));
        assert!(registry.contains("lang"));
    }

    // P2-34 Phase 2: Tests for scope.* fields

    #[test]
    fn test_scope_type_field_exists() {
        let registry = FieldRegistry::with_core_fields();
        let field = registry.get("scope.type");
        assert!(field.is_some());

        let scope_type = field.unwrap();
        assert_eq!(scope_type.name, "scope.type");
        assert!(!scope_type.indexed); // scope.* fields are NOT indexed
        assert!(matches!(scope_type.field_type, FieldType::Enum(_)));
        assert!(scope_type.supports_operator(&Operator::Equal));
        assert!(scope_type.supports_operator(&Operator::Regex));
    }

    #[test]
    fn test_scope_name_field_exists() {
        let registry = FieldRegistry::with_core_fields();
        let field = registry.get("scope.name");
        assert!(field.is_some());

        let scope_name = field.unwrap();
        assert_eq!(scope_name.name, "scope.name");
        assert!(!scope_name.indexed); // scope.* fields are NOT indexed
        assert!(matches!(scope_name.field_type, FieldType::String));
        assert!(scope_name.supports_operator(&Operator::Equal));
        assert!(scope_name.supports_operator(&Operator::Regex));
    }

    #[test]
    fn test_scope_parent_field_exists() {
        let registry = FieldRegistry::with_core_fields();
        let field = registry.get("scope.parent");
        assert!(field.is_some());

        let scope_parent = field.unwrap();
        assert_eq!(scope_parent.name, "scope.parent");
        assert!(!scope_parent.indexed); // scope.* fields are NOT indexed
        assert!(matches!(scope_parent.field_type, FieldType::String));
        assert!(scope_parent.supports_operator(&Operator::Equal));
        assert!(scope_parent.supports_operator(&Operator::Regex));
    }

    #[test]
    fn test_scope_ancestor_field_exists() {
        let registry = FieldRegistry::with_core_fields();
        let field = registry.get("scope.ancestor");
        assert!(field.is_some());

        let scope_ancestor = field.unwrap();
        assert_eq!(scope_ancestor.name, "scope.ancestor");
        assert!(!scope_ancestor.indexed); // scope.* fields are NOT indexed
        assert!(matches!(scope_ancestor.field_type, FieldType::String));
        assert!(scope_ancestor.supports_operator(&Operator::Equal));
        assert!(scope_ancestor.supports_operator(&Operator::Regex));
    }
}
