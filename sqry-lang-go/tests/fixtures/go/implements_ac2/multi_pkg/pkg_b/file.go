// Package pkg_b hosts the concrete File type for the AC-2
// cross-package subcase: pkg_b.File implements pkg_a.Reader.
package pkg_b

type File struct{}

func (f File) Read(p []byte) (int, error) {
	_ = p
	return 0, nil
}
