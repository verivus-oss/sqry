class Ledger {
  /** @type {number} */
  mutableField = 1;
  static staticField = 2;
  #privateField = 3;
  sharedName = 4;

  constructor() {
    this.constructorField = 5;
  }
}

class Archive {
  sharedName = 1;
}

export function readLedger(ledger) {
  return ledger.mutableField + Ledger.staticField;
}
