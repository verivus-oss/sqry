# Advanced: closures, destructuring, comprehensions, except, with

def closure_capture():
    x = 10
    f = lambda y: x + y
    return f(1)

def destructuring_tuple():
    pair = (1, 2)
    a, b = pair
    return a + b

def destructuring_list():
    data = [1, 2, 3]
    first, *rest = data
    return first

def try_except_binding():
    try:
        result = 1 / 0
    except ZeroDivisionError as err:
        msg = str(err)
    return msg

def with_binding():
    with open("test.txt") as f:
        data = f.read()
    return data

def walrus_operator():
    values = [1, 2, 3, 4, 5]
    filtered = [y for x in values if (y := x * 2) > 4]
    return filtered

def nested_function():
    outer = 10
    def inner():
        return outer + 1
    return inner()

def comprehension_var():
    items = [1, 2, 3]
    result = [item * 2 for item in items]
    return result
