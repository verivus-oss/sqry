// Browser client that talks to the backend server.

async function listItems() {
    const response = await fetch("/api/items");
    return response.json();
}

async function addItem(name) {
    const response = await fetch("/api/items", {
        method: "POST",
        body: JSON.stringify({ name }),
    });
    return response.json();
}

async function removeItem(itemId) {
    await fetch(`/api/items/${itemId}`, { method: "DELETE" });
}

module.exports = { listItems, addItem, removeItem };
