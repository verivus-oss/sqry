// SPDX-License-Identifier: MIT
package crosslanguage.realistic;

public class SessionState {
    public String mutableField;
    public final String immutableField = "fixed";
    public static int staticField;
    private boolean privateField;
    public int sharedName;
}

class AuditState {
    public int sharedName;
}
