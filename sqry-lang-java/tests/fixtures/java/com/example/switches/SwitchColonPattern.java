package com.example.switches;

class SwitchColonPattern {
    void colonPattern(Object obj) {
        switch (obj) {
            case String s:
                System.out.println(s);
                break;
            case Integer i:
                System.out.println(i);
                break;
            default:
                break;
        }
    }
}
