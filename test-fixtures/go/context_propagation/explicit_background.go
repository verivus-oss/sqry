// T3.7 AC-T3.7-5 fixture: explicit context.Background() is still a leak.
// Caller has ctx, callee accepts ctx, the call explicitly substitutes
// context.Background(). The span-text regex matches the literal, so
// the leak is reported (NOT silently auto-suppressed).
package contextpropagation

import "context"

func BgCallee(ctx context.Context, key string) (string, error) {
	_ = ctx
	return key, nil
}

func BgCaller(ctx context.Context) (string, error) {
	_ = ctx
	return BgCallee(context.Background(), "k")
}
