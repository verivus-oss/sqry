// Minimal UI binding layer that drives the client.

const client = require("./client");

async function refreshList() {
    const items = await client.listItems();
    return items.length;
}

async function submit(name) {
    const result = await client.addItem(name);
    return result.status;
}

module.exports = { refreshList, submit };
