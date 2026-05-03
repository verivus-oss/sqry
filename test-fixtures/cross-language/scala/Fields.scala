class Ledger {
  var mutableField: Int = 0
  val immutableField: Int = 1
  private var privateField: String = ""
  var sharedName: Int = 0
}

object Ledger {
  var staticField: Int = 0
}

class Archive {
  var sharedName: Int = 0
}
