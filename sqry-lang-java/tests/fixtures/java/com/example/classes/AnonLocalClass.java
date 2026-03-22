package com.example.classes;

class AnonLocalClass {
    void memberBeatsCapture() {
        int x = 1;
        Runnable r = new Runnable() {
            int x = 2; // member shadows captured x
            public void run() {
                System.out.println(x); // resolves to member x, not captured
            }
        };
    }

    void captureWins() {
        int y = 1;
        Runnable r = new Runnable() {
            public void run() {
                System.out.println(y); // captures outer y (no member y)
            }
        };
    }
}
