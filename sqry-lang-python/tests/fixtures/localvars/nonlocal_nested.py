def outer():
    total = 0

    def inner():
        nonlocal total
        total = total + 1
        return total

    inner()
    return total
