// T3.7 AC-T3.7-3 fixture: unthreaded goroutine.
// `go Expensive()` launches a goroutine and passes no ctx, despite
// Expensive accepting context.Context. Sqry should classify exactly
// one ContextLeak{mode: UnthreadedGoroutine}.
package contextpropagation

import "context"

func Expensive(ctx context.Context) {
	_ = ctx
}

func LaunchExpensive() {
	go Expensive(context.TODO())
}
