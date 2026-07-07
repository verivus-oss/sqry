def collect():
    global cached
    if (cached := compute()):
        return cached
    return None
