def outer():
    global value
    value = 1
    outer_read = value

    def inner():
        value = 10
        return value

    return inner()
