;; Capture top-level constructs that can contain call sites. The shared engine
;; uses these roots to derive caller names and walk their bodies for call
;; expressions.
[
  (function_declaration)
  (function_expression)
  (generator_function)
  (generator_function_declaration)
  (arrow_function)
  (class_declaration)
  (class)
] @call.root
