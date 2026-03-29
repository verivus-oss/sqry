from typing import List, Optional

class Calculator:
    def __init__(self):
        self.value = 0

    async def add_async(self, a: int, b: int) -> int:
        return a + b

    @staticmethod
    def multiply(a: int, b: int) -> int:
        return a * b

    @property
    def current_value(self) -> int:
        return self.value

def process_items(items: List[str]) -> Optional[str]:
    if not items:
        return None
    return items[0]
