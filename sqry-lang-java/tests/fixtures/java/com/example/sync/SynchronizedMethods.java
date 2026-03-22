// Test fixture: Synchronized methods
// Tests: synchronized method detection

package com.example.sync;

public class SynchronizedMethods {

    private int counter = 0;
    private final Object lock = new Object();

    public synchronized void incrementSync() {
        counter++;
    }

    public synchronized int getCounterSync() {
        return counter;
    }

    public static synchronized void staticSyncMethod() {
        System.out.println("Static synchronized");
    }

    public void methodWithSyncBlock() {
        synchronized (lock) {
            counter++;
        }
    }

    public void normalMethod() {
        incrementSync();
        int value = getCounterSync();
        staticSyncMethod();
        if (value > 0) {
            counter = value;
        }
    }

    public static void main(String[] args) {
        SynchronizedMethods obj = new SynchronizedMethods();
        obj.incrementSync();
        obj.methodWithSyncBlock();
        obj.normalMethod();
        staticSyncMethod();
    }
}
