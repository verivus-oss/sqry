# Python async function test fixture
import asyncio

async def fetch_data():
    """Async function that fetches data."""
    await asyncio.sleep(1)
    return "data"

def sync_function():
    """Regular synchronous function."""
    return "sync"
