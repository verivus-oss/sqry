// Package fx is the AC-3 fixture per 01_SPEC.md §7 AC-3: pointer-vs-value
// receiver discrimination. BufferV satisfies Writer via the value
// method set; BufferP satisfies Writer only via the pointer method
// set. The pass must emit Implements(BufferV → Writer) and
// Implements(*BufferP → Writer), but NOT Implements(BufferP → Writer).
package fx

type Writer interface {
	Write(p []byte) (int, error)
}

type BufferV struct{}

func (b BufferV) Write(p []byte) (int, error) {
	_ = p
	return 0, nil
}

type BufferP struct{}

func (b *BufferP) Write(p []byte) (int, error) {
	_ = p
	return 0, nil
}
