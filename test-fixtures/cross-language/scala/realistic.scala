// SPDX-License-Identifier: MIT
package crosslanguage

case class SessionState(var mutableField: String, immutableField: String) {
  private var privateField: Boolean = false
  var sharedName: Int = 0
}

object SessionState {
  val staticField: Int = 1
}

class AuditState {
  var sharedName: Int = 0
}
