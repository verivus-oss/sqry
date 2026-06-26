// Hand-written Apex sample for body-shape descriptor coverage.
// Exercises the real control-flow constructs the Apex grammar parses:
// branch, loop (classic + enhanced), match (switch), try/catch/throw/finally,
// return, break/continue, and call (method + object creation).
public class OrderProcessor {
    public Integer process(Integer count, String region) {
        Integer total = count;
        if (total > 0) {
            total = total + 1;
        } else {
            total = 0;
        }

        for (Integer i = 0; i < count; i++) {
            if (i == 3) {
                continue;
            }
            if (i > 10) {
                break;
            }
            total += i;
        }

        while (total > 100) {
            total--;
        }

        for (Account a : lookupAccounts(region)) {
            System.debug(a);
        }

        switch on total {
            when 1 {
                total = 1;
            }
            when else {
                total = 2;
            }
        }

        try {
            total = compute(total);
        } catch (Exception e) {
            throw new ProcessingException(e);
        } finally {
            cleanup();
        }

        return total;
    }

    private Integer compute(Integer value) {
        return value * 2;
    }
}
