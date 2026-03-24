package com.example

import groovy.transform.CompileStatic

@CompileStatic
class Calculator {
    private int value = 0

    Calculator(int initial) {
        this.value = initial
    }

    int add(int x) {
        value + x
    }

    int subtract(int x) {
        value - x
    }
}

trait Logging {
    void log(String message) {
        println "[LOG] $message"
    }
}

class Service implements Logging {
    void process() {
        log("Processing...")
    }
}
