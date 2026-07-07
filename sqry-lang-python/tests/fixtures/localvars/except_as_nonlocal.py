def outer():
    err = None

    def inner():
        nonlocal err
        try:
            pass
        except Exception as err:
            use(err)

    inner()
    return err
