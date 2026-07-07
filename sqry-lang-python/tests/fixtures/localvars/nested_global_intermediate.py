value = 100


def outer():
    value = 1

    def inner():
        global value
        return value

    return inner()
