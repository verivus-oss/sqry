// Hand-written control-flow sample for the Scala body-shape descriptor coverage
// test. Exercises branch, loop, match, try/catch, throw, return, and a call so
// the canonical CfBucket histogram is non-empty.

object Classifier {
  def compute(value: Int): Int = value + 1

  def classify(n: Int, label: String): Int = {
    var total = 0
    if (n > 0) {
      total = compute(n)
    } else {
      total = 0
    }

    while (total < 100) {
      total = total + 1
    }

    val tag = n match {
      case 0 => "zero"
      case _ => "other"
    }

    try {
      if (tag == "zero") {
        throw new RuntimeException(label)
      }
    } catch {
      case _: RuntimeException => total = -1
    }

    return total
  }
}
