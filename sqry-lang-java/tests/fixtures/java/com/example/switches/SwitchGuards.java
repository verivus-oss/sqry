package com.example.switches;

class SwitchGuards {
    int guardTest(Object obj) {
        return switch (obj) {
            case String s when s.length() > 0 -> s.hashCode();
            case Integer i -> i;
            default -> 0;
        };
    }

    void classicFallthrough(int value) {
        switch (value) {
            case 1:
                int x = 1;
                System.out.println(x);
            case 2:
                System.out.println(x); // x visible due to fallthrough
                break;
            default:
                break;
        }
    }
}
