package com.example.switches;

class SwitchDuplicateDecls {
    void test(int value) {
        switch (value) {
            case 1:
                int x = 1;
                System.out.println(x);
                break;
            case 2:
                int x = 2;
                System.out.println(x);
                break;
            default:
                System.out.println(value);
        }
    }
}
