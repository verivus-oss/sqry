// Hand-written JavaScript sample exercising real control-flow kinds the
// body-shape descriptor buckets. MIT-clean, no vendored sources.

function classify(values, threshold = 0, ...extra) {
    let total = 0;
    for (const value of values) {
        if (value > threshold) {
            total += value;
        } else if (value < 0) {
            continue;
        } else {
            break;
        }
    }
    const squares = values.filter((v) => v > 0).map((v) => v * v);
    while (total > 1000) {
        total -= 1;
    }
    try {
        helper(total);
    } catch (err) {
        throw new Error("classify failed");
    }
    switch (total) {
        case 0:
            return squares.length;
        default:
            return total;
    }
}

async function fetchValue(url) {
    const result = await client.get(url);
    const doubler = (n) => n * 2;
    return String(doubler(result));
}
