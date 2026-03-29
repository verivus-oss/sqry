// No false positive tests — type names, member access, import identifiers

function typeNames(): void {
    let x: number = 5;
    console.log(x);
}

function memberAccess(): void {
    const s = "hello";
    let len = s.length;
    console.log(len);
}

class MyClass {
    method(): void {
        let x = 5;
        console.log(x);
    }
}
