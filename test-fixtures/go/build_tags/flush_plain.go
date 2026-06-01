package buildtags

// FlushPlain is the unconstrained baseline: no //go:build, no // +build,
// no filename suffix, no `import "C"`. Per 02_DESIGN §10.3 / §4.3.b the
// expected cfg_condition is None (the constraint stamping pass leaves
// the node's metadata untouched).
func FlushPlain() {}
