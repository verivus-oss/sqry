// T3.7 fixture: intentional break.
// Documents that sqry surfaces the leak finding even when the author
// "knew" they were dropping ctx — sqry does not auto-suppress based on
// intent comments. The analyst sees the finding and decides; sqry's
// job is to surface, not adjudicate.
package contextpropagation

import "context"

func Background(ctx context.Context, key string) (string, error) {
	_ = ctx
	return key, nil
}

// IntentionalBreak deliberately drops the caller's ctx because the
// background job must outlive the request. Sqry should still report
// this as a leak — silently suppressing would hide the same shape in
// other callers who *did* make a mistake.
func IntentionalBreak(ctx context.Context) (string, error) {
	_ = ctx
	return Background(context.Background(), "k")
}
