config = 0


def use_global():
    global config
    config = config + 1
    return config
