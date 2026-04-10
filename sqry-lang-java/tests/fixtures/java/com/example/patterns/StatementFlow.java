package com.example.patterns;

class StatementFlow {
    void afterIf(Object obj) {
        if (!(obj instanceof String ifString)) {
            return;
        }
        System.out.println(ifString);
    }

    void afterWhile(Object obj) {
        while (!(obj instanceof String whileString)) {
            obj = "ready";
        }
        System.out.println(whileString);
    }

    void afterDoWhile(Object obj) {
        do {
            obj = "ready";
        } while (!(obj instanceof String doString));
        System.out.println(doString);
    }

    void afterFor(Object obj) {
        for (; !(obj instanceof String forString); obj = "ready") {
            obj = "ready";
        }
        System.out.println(forString);
    }

    void afterWhileWithSwitchBreak(Object obj, int value) {
        while (!(obj instanceof String switchString)) {
            switch (value) {
                case 1:
                    break;
                default:
                    value = 1;
            }
            obj = "ready";
        }
        System.out.println(switchString);
    }

    void afterWhileWithLabeledBlockBreak(Object obj) {
        while (!(obj instanceof String labeledBlockString)) {
            INNER: {
                if (obj == null) {
                    break INNER;
                }
            }
            obj = "ready";
        }
        System.out.println(labeledBlockString);
    }

    void afterWhileWithLabeledLoopBreak(Object obj) {
        OUTER:
        while (!(obj instanceof String outerLabeledString)) {
            switch (obj.hashCode()) {
                case 0:
                    break OUTER;
                default:
                    obj = "ready";
            }
        }
        System.out.println(outerLabeledString);
    }
}
