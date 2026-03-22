// Basic variable declaration + usage tests

function simpleVar() {
    let x = 10;
    let y = x + 1;
    return y;
}

function constVar() {
    const count = 42;
    console.log(count);
}

function multipleDeclarators() {
    let a = 1, b = 2;
    let c = a + b;
    return c;
}

function paramRef(name, age) {
    const result = name;
    console.log(age);
    return result;
}
