import { helper } from './helper.js';

export class Calculator {
    constructor() {
        this.value = 0;
    }

    async addAsync(a, b) {
        return await Promise.resolve(a + b);
    }

    multiply = (a, b) => a * b;
}

export function add(a, b) {
    return a + b;
}

const subtract = (a, b) => a - b;
