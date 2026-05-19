/*
 * SPDX-License-Identifier: MIT
 *
 * Synthetic fixture for sqry C indirect-call precision (Phase A).
 *
 * NEGATIVE control. Exercises the §3.1.1 boundary: only FUNCTION
 * identifiers participate in the address-taken set. Taking the address
 * of a non-function (`&some_var`) MUST NOT produce an address-taken
 * mark on any function. There are no callable targets in this file —
 * pass5b must not surface any address-taken flag.
 */

static int some_var = 0;

void init(void) {
    int *p = &some_var;
    (void)p;
}
