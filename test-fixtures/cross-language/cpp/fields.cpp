struct Ledger {
public:
    int mutableField;
    const int immutableField = 1;
    static int staticField;
    int sharedName;
private:
    int privateField;
};

int Ledger::staticField = 0;

struct Archive {
    int sharedName;
};

int readLedger(const Ledger &ledger) {
    return ledger.mutableField + Ledger::staticField;
}
