def outer():
    handle = None

    def inner():
        nonlocal handle
        with open("f") as handle:
            use(handle)

    inner()
    return handle
