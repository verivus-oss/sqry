// Advanced patterns: arrow functions, destructuring, try/catch, switch

function arrowCapture(): void {
    let x = 10;
    const fn1 = () => x;
    console.log(fn1());
}

function tryCatchVar(): void {
    try {
        let x = 42;
        console.log(x);
    } catch (error) {
        console.log(error);
    }
}

function switchVar(): void {
    let x = 3;
    switch (x) {
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

function destructuringArray(): void {
    const arr = [1, 2, 3];
    const [a, b, c] = arr;
    console.log(a);
    console.log(b);
    console.log(c);
}

function destructuringObject(): void {
    const obj = { name: "Alice", age: 30 };
    const { name, age } = obj;
    console.log(name);
    console.log(age);
}

function destructuringRename(): void {
    const obj = { x: 1, y: 2 };
    const { x: renamed } = obj;
    console.log(renamed);
}

function restParams(...args: number[]): void {
    console.log(args);
}
