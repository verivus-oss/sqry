// AC-2 (cross-file, same package): the Reader interface lives in a.go.
package fx

type Reader interface {
	Read(p []byte) (int, error)
}
