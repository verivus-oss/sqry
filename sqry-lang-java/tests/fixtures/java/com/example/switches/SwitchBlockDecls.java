package com.example.switches;

class SwitchBlockDecls {
    int arrowDecls(int value) {
        return switch (value) {
            case 1 -> {
                int y = 1;
                yield y;
            }
            case 2 -> {
                int y = 2;
                yield y;
            }
            default -> 0;
        };
    }
}
