// SPDX-License-Identifier: MIT
public class AccountState {
    public String mutableField;
    public final String immutableField = 'fixed';
    public static Integer staticField;
    private String privateField;
    public Integer sharedName;

    public String describe() {
        return mutableField;
    }
}

public class AuditState {
    public Integer sharedName;
}
