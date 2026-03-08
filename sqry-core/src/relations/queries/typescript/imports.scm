;; Capture every TypeScript `import_statement`. The shared engine handles the
;; finer-grained specifier analysis so the query only needs to surface the
;; top-level statement node.
(import_statement) @import.statement
