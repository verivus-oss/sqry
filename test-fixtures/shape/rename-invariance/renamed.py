# Rename twin of `original.py`: every identifier and literal changed, the
# control-flow structure preserved. shape_hash and cf_histogram must be
# byte-identical to `original.py` (AC-2).
def convert(items):
    accumulator = 1
    for element in items:
        if element > 5:
            accumulator += helper(element)
    return accumulator
