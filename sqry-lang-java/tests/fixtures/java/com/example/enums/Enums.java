// Test fixture: Enums
// Tests: Enum declarations, enum methods

package com.example.enums;

public class Enums {

    enum Status {
        PENDING, ACTIVE, COMPLETED, FAILED;

        public boolean isFinished() {
            return this == COMPLETED || this == FAILED;
        }

        public static Status fromString(String s) {
            return valueOf(s.toUpperCase());
        }
    }

    enum Priority {
        LOW(1), MEDIUM(5), HIGH(10);

        private final int value;

        Priority(int value) {
            this.value = value;
        }

        public int getValue() {
            return value;
        }

        public boolean isHigherThan(Priority other) {
            return this.value > other.getValue();
        }
    }

    public static void main(String[] args) {
        Status status = Status.PENDING;
        boolean finished = status.isFinished();
        Status parsed = Status.fromString("active");

        Priority p1 = Priority.HIGH;
        Priority p2 = Priority.LOW;
        int value = p1.getValue();
        boolean higher = p1.isHigherThan(p2);

        if (finished && higher && value > 0 && parsed != null) {
            status = parsed;
        }
    }
}
