// Basic local variable declarations and usages

function simpleVar(): void {
    let x = 10;
    let y = x + 1;
    console.log(y);
}

function constVar(): void {
    const count = 42;
    console.log(count);
}

function multipleDeclarators(): void {
    let a = 1, b = 2;
    let c = a + b;
    console.log(c);
}

function paramRef(name: string, age: number): void {
    const result = name;
    console.log(result);
    console.log(age);
}
