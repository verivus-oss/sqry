;; Go exports (capitalized top-level declarations)

(function_declaration
  name: (identifier) @export.name) @export.declaration

(method_declaration
  name: (identifier) @export.name) @export.declaration

(type_declaration
  (type_spec
    name: (type_identifier) @export.name)) @export.declaration

(const_declaration
  (const_spec
    name: (identifier) @export.name)) @export.declaration

(var_declaration
  (var_spec
    name: (identifier) @export.name)) @export.declaration
