// T3.6 AC-T3.6-4 fixture: `Unwrap() []error` method (slice-literal body
// shape). Sqry should emit one Wraps{UnwrapMultiMethod, Some(i)} edge
// per slice-literal element.
package errorwrapping

type multi struct {
	a error
	b error
}

func (m *multi) Error() string { return "multi" }

func (m *multi) Unwrap() []error {
	return []error{m.a, m.b}
}
