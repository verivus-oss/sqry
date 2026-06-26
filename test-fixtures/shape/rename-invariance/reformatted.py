# Reformat twin of `original.py`: identical identifiers and literals, comments
# and blank lines added. The walker skips comments and whitespace, so the
# descriptor is unchanged while body_hash differs (AC-3).
def transform(values):
    # running total of the scaled positive values
    total = 0

    for value in values:
        # only positive contributions are scaled
        if value > 0:
            total += scale(value)  # accumulate

    return total  # done
