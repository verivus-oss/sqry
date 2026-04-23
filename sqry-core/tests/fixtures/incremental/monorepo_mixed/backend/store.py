"""Simple in-memory store used by the Flask server."""

from typing import Dict, List


class ItemStore:
    def __init__(self) -> None:
        self._items: Dict[int, str] = {}
        self._next_id: int = 1

    def all(self) -> List[Dict[str, object]]:
        return [{"id": i, "name": n} for i, n in self._items.items()]

    def add(self, name: str) -> int:
        item_id = self._next_id
        self._items[item_id] = name
        self._next_id += 1
        return item_id

    def remove(self, item_id: int) -> None:
        self._items.pop(item_id, None)
