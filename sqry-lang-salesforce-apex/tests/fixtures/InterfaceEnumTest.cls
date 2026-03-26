/**
 * Test file for interfaces and enums
 */

// Interface definition
public interface Payable {
    Decimal calculatePayment();
    void processPayment(Decimal amount);
}

// Second interface
public interface Comparable {
    Integer compareTo(Object other);
}

// Enum definition
public enum Status {
    PENDING,
    APPROVED,
    REJECTED,
    CANCELLED
}

// Another enum
public enum Priority {
    LOW,
    MEDIUM,
    HIGH,
    CRITICAL
}

// Interface extending another interface
public interface ExtendedPayable extends Payable {
    void refundPayment(Decimal amount);
}

// Class implementing interfaces
public class Invoice implements Payable, Comparable {
    public Status status;
    public Priority priority;

    public Decimal calculatePayment() {
        return 100.0;
    }

    public void processPayment(Decimal amount) {
        this.status = Status.APPROVED;
    }

    public Integer compareTo(Object other) {
        return 0;
    }
}

// Class with inheritance
public class PremiumInvoice extends Invoice {
    public Decimal calculatePayment() {
        return 200.0;
    }
}

// Class with both inheritance and interface
public class AdvancedInvoice extends Invoice implements Payable {
    public void processPayment(Decimal amount) {
        this.status = Status.PENDING;
    }
}
