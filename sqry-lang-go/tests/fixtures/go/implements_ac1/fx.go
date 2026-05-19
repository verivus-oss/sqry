// Package fx is the AC-1 fixture per 01_SPEC.md §7 AC-1: single-method
// implicit interface satisfaction. *File satisfies Reader via the
// pointer-form method set (Read has *File receiver). go build ./...
// must succeed against this file.
package fx

type Reader interface {
	Read(p []byte) (int, error)
}

type File struct{}

func (f *File) Read(p []byte) (int, error) {
	_ = p
	return 0, nil
}
