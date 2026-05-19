// Package fx is the AC-7 fixture per 01_SPEC.md §7 AC-7: interface
// mismatch produces no edge. NotACloser exposes Open() error but not
// Close() error, so the Implements(NotACloser → Closer) edge must NOT
// be emitted.
package fx

type Closer interface {
	Close() error
}

type NotACloser struct{}

func (NotACloser) Open() error { return nil }
