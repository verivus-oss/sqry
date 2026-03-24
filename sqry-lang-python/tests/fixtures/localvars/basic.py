# Basic: simple assignment, parameters, constants

def simple_var():
    x = 10
    y = x + 1
    return y

def const_binding():
    count = 42
    result = count + 1
    return result

def mutable_var():
    x = 10
    x = x + 1
    return x

def param_ref(name, age):
    result = name
    total = age + 1
    return result
