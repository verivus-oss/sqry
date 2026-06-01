// T3.7 AC-T3.7-2 fixture: ctx properly threaded, no leak.
// Sqry should report zero leaks — Caller's ctx is passed through.
package contextpropagation

import "context"

func ThreadedCallee(ctx context.Context, key string) (string, error) {
	_ = ctx
	return key, nil
}

func ThreadedCaller(ctx context.Context) (string, error) {
	return ThreadedCallee(ctx, "k")
}
