// Test fixture: Instance method calls
// Tests: obj.method(), this.method(), method chaining

package com.example.instance;

public class InstanceMethods {

    private String name;
    private int value;

    public InstanceMethods(String name, int value) {
        this.name = name;
        this.value = value;
    }

    public String getName() {
        return name;
    }

    public int getValue() {
        return value;
    }

    public void setValue(int value) {
        this.value = value;
    }

    public InstanceMethods increment() {
        this.value++;
        return this;
    }

    public InstanceMethods setName(String name) {
        this.name = name;
        return this;
    }

    public void display() {
        String n = this.getName();
        int v = this.getValue();
        System.out.println(n + ": " + v);
    }

    public static void main(String[] args) {
        InstanceMethods obj = new InstanceMethods("test", 10);
        obj.display();

        obj.increment().increment().setName("updated").display();

        String name = obj.getName();
        int value = obj.getValue();

        if (name != null) {
            obj.setName(name);
            obj.setValue(value);
        }
    }
}
