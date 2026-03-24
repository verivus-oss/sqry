;; Capture top-level constructs that own call sites. Mirrors the TypeScript
;; query but targets the JavaScript grammar.
[
  (function_declaration)
  (function_expression)
  (generator_function)
  (generator_function_declaration)
  (arrow_function)
  (class_declaration)
  (class)
] @call.root
