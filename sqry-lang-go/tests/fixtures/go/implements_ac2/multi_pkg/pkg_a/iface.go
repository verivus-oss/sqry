// Package pkg_a hosts the Reader interface for the AC-2 cross-package
// subcase: pkg_b.File implements pkg_a.Reader.
package pkg_a

type Reader interface {
	Read(p []byte) (int, error)
}
