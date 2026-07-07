def outer():
    value = 1

    def inner():
        global value
        value = 2
        return value

    inner()
    return value
