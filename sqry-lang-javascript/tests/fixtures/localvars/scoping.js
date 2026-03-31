// Scoping and loop variable tests

function shadowedVar() {
    let x = 1;
    {
        let x = 2;
        console.log(x);
    }
    console.log(x);
}

function forLoopVar() {
    for (let i = 0; i < 10; i++) {
        console.log(i);
    }
}

function forOfVar() {
    const items = [1, 2, 3];
    for (const item of items) {
        console.log(item);
    }
}

function forInVar() {
    const obj = { a: 1, b: 2 };
    for (const key in obj) {
        console.log(key);
    }
}

function multipleRefs() {
    let x = 1;
    let y = x + x;
    let z = x + y;
    return z;
}
