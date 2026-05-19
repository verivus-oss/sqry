/*
 * SPDX-License-Identifier: MIT
 *
 * Synthetic hand-written fixture for sqry C indirect-call precision
 * (Phase A). NOT vendored from upstream — purely a stand-in shaped
 * like Linux's eBPF struct_ops `tcp_congestion_ops` so we can
 * exercise the binding plane's "positional + designated mix" pattern
 * outside ext4 idioms (DESIGN §13.3).
 *
 * Each function below is a synthetic stub. The binding table
 * `cubictcp` mixes designated initializers (the common pattern in
 * upstream eBPF struct_ops) with a few positional placeholders to
 * cover both classifier paths in SPEC §3.1.1.
 *
 * Expected resolutions are encoded in `expected.json`.
 */

#include <stddef.h>

struct sock;

struct tcp_congestion_ops {
    void (*init)(struct sock *sk);
    void (*release)(struct sock *sk);
    unsigned int (*ssthresh)(struct sock *sk);
    void (*cong_avoid)(struct sock *sk, unsigned int ack, unsigned int acked);
    void (*set_state)(struct sock *sk, unsigned char new_state);
    void (*cwnd_event)(struct sock *sk, int ev);
    void (*in_ack_event)(struct sock *sk, unsigned int flags);
    unsigned int (*undo_cwnd)(struct sock *sk);
    void (*pkts_acked)(struct sock *sk, const void *sample);
};

/* Synthetic implementations. Identifier names mirror the upstream
 * tcp_cubic naming so reviewers familiar with Linux's net/ipv4/tcp_cubic.c
 * recognise the pattern, but the bodies are stubs. */

static void cubic_init(struct sock *sk) {
    (void)sk;
}

static void cubic_release(struct sock *sk) {
    (void)sk;
}

static unsigned int cubic_recalc_ssthresh(struct sock *sk) {
    (void)sk;
    return 0;
}

static void cubic_cong_avoid(struct sock *sk, unsigned int ack, unsigned int acked) {
    (void)sk;
    (void)ack;
    (void)acked;
}

static void cubic_state(struct sock *sk, unsigned char new_state) {
    (void)sk;
    (void)new_state;
}

static void cubic_cwnd_event(struct sock *sk, int ev) {
    (void)sk;
    (void)ev;
}

static void cubic_acked(struct sock *sk, const void *sample) {
    (void)sk;
    (void)sample;
}

/* Binding table. Designated initializers dominate; the trailing two
 * slots are positional NULLs so the positional-initializer classifier
 * path is non-trivially exercised against a function-pointer field
 * sequence. */
static const struct tcp_congestion_ops cubictcp = {
    .init        = cubic_init,
    .release     = cubic_release,
    .ssthresh    = cubic_recalc_ssthresh,
    .cong_avoid  = cubic_cong_avoid,
    .set_state   = cubic_state,
    .cwnd_event  = cubic_cwnd_event,
    .pkts_acked  = cubic_acked,
};
