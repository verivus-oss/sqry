# Hand-written Python sample exercising the real control-flow kinds the
# body-shape descriptor buckets. MIT-clean, no vendored sources.


def classify(values, threshold=0, *extra, **opts):
    total = 0
    for value in values:
        if value > threshold:
            total += value
        elif value < 0:
            continue
        else:
            break
    squares = [v * v for v in values if v > 0]
    try:
        with open("data") as handle:
            payload = handle.read()
    except OSError as err:
        raise RuntimeError("read failed") from err
    match total:
        case 0:
            return squares
        case _:
            return payload
    helper(total)


async def fetch(url) -> str:
    result = await client.get(url)
    yield result
    doubler = lambda n: n * 2
    return doubler(result)
