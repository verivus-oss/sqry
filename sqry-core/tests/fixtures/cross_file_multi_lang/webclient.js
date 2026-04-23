async function fetchUsers() {
    const response = await fetch('/api/users');
    return response.json();
}

async function createUser(name) {
    const response = await fetch('/api/users', {
        method: 'POST',
        body: JSON.stringify({ name }),
    });
    return response.json();
}

module.exports = { fetchUsers, createUser };
