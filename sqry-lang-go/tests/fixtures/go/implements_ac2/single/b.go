// AC-2 (cross-file, same package): the concrete File type lives in
// b.go. Satisfaction must hold even though Reader is declared in
// a.go and File in b.go.
package fx

type File struct{}

func (f File) Read(p []byte) (int, error) {
	_ = p
	return 0, nil
}
