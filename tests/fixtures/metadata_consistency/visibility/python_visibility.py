# Python visibility test fixture

def public_function():
    """Public function (no underscore prefix)."""
    return "public"

def _private_function():
    """Private function (single underscore prefix)."""
    return "private"
