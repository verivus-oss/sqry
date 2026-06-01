// T3.7 AC-T3.7-4 fixture: HTTP handler leak.
// `H` is recognised by signature shape (http.ResponseWriter, *http.Request).
// It calls `Save` (ctx-accepting callee) but never threads `r.Context()`.
// Sqry should classify exactly one ContextLeak{mode: HttpHandlerLeak}.
package contextpropagation

import (
	"context"
	"net/http"
)

func Save(ctx context.Context, body string) error {
	_ = ctx
	_ = body
	return nil
}

func H(w http.ResponseWriter, r *http.Request) {
	_ = w
	_ = r
	_ = Save(context.TODO(), "data")
}
