# No false positives: types, imports, attributes, calls, decorators

import os
from pathlib import Path

def type_names(x: int, y: str) -> bool:
    # int, str, bool are type annotations, NOT local variables
    return True

def attribute_access():
    obj = {"key": "value"}
    result = obj.get("key")
    return result

def call_targets():
    result = len([1, 2, 3])
    text = str(result)
    return text

class MyClass:
    def method(self):
        return self.value
