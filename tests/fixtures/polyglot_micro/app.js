// Minimal JavaScript file for polyglot test fixture
// This file provides simple functions to verify JavaScript node extraction

function helloJavaScript() {
    return "Hello from JavaScript";
}

class DataStore {
    constructor(name) {
        this.name = name;
        this.items = [];
    }

    addItem(item) {
        this.items.push(item);
    }

    getItems() {
        return this.items;
    }
}

export { helloJavaScript, DataStore };
