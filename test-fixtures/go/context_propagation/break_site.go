// T3.7 AC-T3.7-1 fixture: classic break-site.
// `Caller` accepts context.Context, `Callee` accepts context.Context,
// but the call passes zero context args. Sqry should classify this as
// exactly one ContextLeak{mode: BreakSite}.
package contextpropagation

import "context"

func Callee(ctx context.Context, key string) (string, error) {
	_ = ctx
	return key, nil
}

func Caller(ctx context.Context) (string, error) {
	_ = ctx
	return Callee(context.TODO(), "k")
}
