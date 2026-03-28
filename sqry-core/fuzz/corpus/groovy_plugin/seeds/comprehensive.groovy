class Calculator {
    def add(a, b) {
        return a + b
    }

    static multiply(a, b) {
        a * b
    }
}

def closure = { x, y ->
    x + y
}

task build {
    doLast {
        println 'Building...'
    }
}
