"""Module-level ctypes usage.

The call below sits at module level, with no enclosing function, so it takes
the synthetic `<module>` caller branch of `get_ffi_caller_node_id`. That is the
one site where this plugin's synthetic context span reaches a mint, which makes
this file the corpus's only observer of whether a whole-file pseudo-body enters
the body-hash and shape planes.
"""

import ctypes

libm = ctypes.CDLL("libm.so.6")


def accumulate(values):
    total = 0.0
    for value in values:
        if value > 0:
            total += libm.sqrt(ctypes.c_double(value))
        else:
            total -= 1.0
    return total


def scale(values, factor):
    out = []
    for value in values:
        if value > 0:
            out.append(value * factor)
        else:
            out.append(0.0)
    return out
