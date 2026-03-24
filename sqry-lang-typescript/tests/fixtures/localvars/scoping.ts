// Scoping, shadowing, and loop variable tests

function shadowedVar(): void {
    let x = 10;
    console.log(x);
    {
        let x = 20;
        console.log(x);
    }
    console.log(x);
}

function forLoopVar(): void {
    for (let i = 0; i < 10; i++) {
        console.log(i);
    }
}

function forOfVar(): void {
    const items = [1, 2, 3];
    for (const item of items) {
        console.log(item);
    }
}

function forInVar(): void {
    const obj = { a: 1, b: 2 };
    for (const key in obj) {
        console.log(key);
    }
}

function multipleRefs(): void {
    let x = 1;
    let y = x + x;
    let z = x + y;
    console.log(z);
}
