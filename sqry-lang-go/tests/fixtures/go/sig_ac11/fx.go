// Package fx is the AC-11 fixture per 01_SPEC.md §7 AC-11: the
// HandlerFunc analog. HandlerFunc is both a named function type
// (T1.3 target: myHandler → HandlerFunc) and a method-set carrier
// (T1.1 target: HandlerFunc → Handler). Both edges must coexist.
package fx

type Request struct{}

type ResponseWriter interface {
	Write([]byte) (int, error)
}

type Handler interface {
	ServeHTTP(ResponseWriter, *Request)
}

type HandlerFunc func(ResponseWriter, *Request)

func (f HandlerFunc) ServeHTTP(w ResponseWriter, r *Request) {
	f(w, r)
}

func myHandler(w ResponseWriter, r *Request) {
	_ = w
	_ = r
}

var _ = HandlerFunc(myHandler)
