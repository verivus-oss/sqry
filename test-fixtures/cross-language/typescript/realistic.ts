// SPDX-License-Identifier: MIT
export class SessionState<T> {
  public mutableField: T;
  public readonly immutableField: string = "fixed";
  public static staticField: number = 0;
  private privateField: boolean = false;
  public sharedName: number = 0;

  constructor(public promotedField: T) {
    this.mutableField = promotedField;
  }
}

export class AuditState {
  public sharedName: number = 0;
}
