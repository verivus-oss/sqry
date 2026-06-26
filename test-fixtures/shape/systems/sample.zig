// Hand-written control-flow sample for the Zig body-shape descriptor coverage
// test. Exercises branch, loop, switch, return, break/continue, call, and
// assignment so the canonical CfBucket histogram is non-empty.

fn compute(value: i32) i32 {
    return value * 2;
}

fn classify(n: i32, items: []const u8) i32 {
    var total: i32 = 0;
    if (n > 0) {
        total = compute(n);
    } else {
        total = 0;
    }

    var i: usize = 0;
    while (i < items.len) {
        if (items[i] == 0) {
            i += 1;
            continue;
        }
        total += items[i];
        if (total > 100) {
            break;
        }
        i += 1;
    }

    for (items) |it| {
        total += it;
    }

    const kind = switch (n) {
        0 => 1,
        else => 2,
    };

    return total + kind;
}
