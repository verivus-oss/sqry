// Package fx is the AC-4 fixture per 01_SPEC.md §7 AC-4 — same-depth
// ambiguity blocks both promotion and implements. The four type
// declarations below are verbatim from golang/go#57352. Per
// 05_TEST_PLAN.md §7.3, the `var _ AB = Foo{}` witness line that
// accompanies the upstream issue is omitted: it fails `go build` by
// design (it is the witness that the ambiguity is real), and the
// pass-level assertion in `ac4_ambiguity` does not depend on it.
package fx

type A interface {
	a()
}

type AB interface {
	A
	b()
}

type Foo struct {
	A
	AB
}

// Sink consumes Foo via its embedded interfaces so the fixture exercises
// the same code paths the upstream example does without depending on
// the `var _ AB = Foo{}` witness (which fails to compile by design).
var _ = func(f Foo) {
	_ = f
}
