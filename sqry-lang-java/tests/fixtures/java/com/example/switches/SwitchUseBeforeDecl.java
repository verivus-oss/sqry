package com.example.switches;

class SwitchUseBeforeDecl {
    void test(int value) {
        switch (value) {
            case 1:
                System.out.println(x); // x not yet declared - no edge
                break;
            case 2:
                int x = 1;
                System.out.println(x);
                break;
        }
    }
}
