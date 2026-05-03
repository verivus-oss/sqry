class Ledger {
  public mutableField: number = 0;
  public readonly immutableField: number = 1;
  public static staticField: number = 0;
  private privateField: string = "";
  public sharedName: number = 0;

  constructor(public promotedField: number) {}
}

class Archive {
  public sharedName: number = 0;
}

export function readLedger(ledger: Ledger): number {
  return ledger.mutableField + Ledger.staticField;
}
