// TypeScript async function test fixture

async function fetchData(): Promise<string> {
    return new Promise((resolve) => {
        setTimeout(() => resolve("data"), 1000);
    });
}

function syncFunction(): void {
    console.log("sync");
}
