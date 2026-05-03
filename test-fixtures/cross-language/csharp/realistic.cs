// SPDX-License-Identifier: MIT
namespace CrossLanguage.Realistic;

public sealed class AccountSnapshot {
    public string MutableField;
    public readonly string ImmutableField = "locked";
    public static int StaticField;
    private decimal PrivateField;
    public int SharedName;

    public AccountSnapshot(string mutableField, decimal privateField) {
        MutableField = mutableField;
        PrivateField = privateField;
    }
}

public sealed class AuditSnapshot {
    public int SharedName;
}
