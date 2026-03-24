// JavaScript HTTP client
async function fetchUsers() {
    const response = await fetch("/api/users");
    return response.json();
}

async function createItem(data) {
    const response = await fetch("/api/items", {
        method: "POST",
        body: JSON.stringify(data),
    });
    return response.json();
}

module.exports = { fetchUsers, createItem };
