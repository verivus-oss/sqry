# AC-2/AC-3 anchor (Python). Same control-flow shape as the Rust anchor:
# branch + loop + call + return + assign. `renamed.py` renames everything,
# `reformatted.py` adds comments and whitespace.
def transform(values):
    total = 0
    for value in values:
        if value > 0:
            total += scale(value)
    return total
