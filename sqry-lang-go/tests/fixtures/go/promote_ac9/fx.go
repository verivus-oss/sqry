// Package fx is the AC-9 fixture per 01_SPEC.md §7 AC-9: type-alias
// embedding promotes through the alias. Verbatim from golang/go#66540
// — type A is an alias for an unnamed struct embedding io.Reader; S
// then embeds A. The pass must promote io.Reader's Read method
// through A onto S.
package fx

import "io"

type A = struct {
	io.Reader
}

type S struct {
	A
}
