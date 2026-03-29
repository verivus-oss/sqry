# Scoping: for loops, no block scope, multiple refs

def for_loop_var():
    items = [1, 2, 3]
    for item in items:
        result = item + 1
    # item is still accessible after the loop (no block scope)
    return item

def multiple_refs():
    x = 1
    y = x + x
    z = x + y
    return z

def no_block_scope():
    if True:
        inner = 42
    # inner is accessible here — no block scope in Python
    return inner

def while_loop_var():
    total = 0
    i = 0
    while i < 10:
        total = total + i
        i = i + 1
    return total
