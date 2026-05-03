// SPDX-License-Identifier: MIT
public final class SessionState {
    public var mutableField: String = ""
    public let immutableField: String = "fixed"
    public static var staticField: Int = 0
    private var privateField: Bool = false
    public var sharedName: Int = 0
}

public final class AuditState {
    public var sharedName: Int = 0
}
