// Advanced: arrow functions, try/catch, switch, destructuring

function arrowCapture() {
    let x = 10;
    const fn1 = () => x;
    return fn1();
}

function tryCatchVar() {
    try {
        throw new Error("test");
    } catch (error) {
        console.log(error);
    }
}

function switchVar(value) {
    switch (value) {
        case 1: {
            let y = 10;
            console.log(y);
            break;
        }
        case 2: {
            let z = 20;
            console.log(z);
            break;
        }
    }
}

function destructuringArray() {
    const arr = [1, 2, 3];
    const [a, b, c] = arr;
    console.log(a);
    console.log(b);
    console.log(c);
}

function destructuringObject() {
    const obj = { name: "test", age: 42 };
    const { name, age } = obj;
    console.log(name);
    console.log(age);
}

function destructuringRename() {
    const obj = { x: 10 };
    const { x: renamed } = obj;
    console.log(renamed);
}

function restParams(...args) {
    console.log(args);
}
