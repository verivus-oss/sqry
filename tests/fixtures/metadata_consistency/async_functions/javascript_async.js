// JavaScript async function test fixture

async function fetchData() {
    return new Promise((resolve) => {
        setTimeout(() => resolve("data"), 1000);
    });
}

function syncFunction() {
    console.log("sync");
}
