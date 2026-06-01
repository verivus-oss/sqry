// T3.6 AC-T3.6-3 fixture: `Unwrap() error` method.
// Sqry should emit one Wraps{UnwrapMethod, None} edge from `*wrap`
// type to the resolved TypeOf target of the `inner` field.
package errorwrapping

type wrap struct {
	inner error
}

func (w *wrap) Error() string {
	return "wrap: " + w.inner.Error()
}

func (w *wrap) Unwrap() error {
	return w.inner
}
