// Hand-written control-flow sample for the Kotlin body-shape descriptor coverage
// test. Exercises branch, loop, when, try/catch, call, assignment, closure, and
// a return jump so the canonical CfBucket histogram is non-empty.

class Classifier {
    fun classify(n: Int, label: String): Int {
        var total = 0
        if (n > 0) {
            total = compute(n)
        } else {
            total = 0
        }

        for (i in 0 until n) {
            if (i == 3) {
                continue
            }
            total += i
        }

        while (total < 100) {
            total += 1
        }

        val tag = when (n) {
            0 -> "zero"
            else -> "other"
        }

        try {
            emit(total)
        } catch (e: Exception) {
            total = -1
        }

        val doubler = { x: Int -> x * 2 }
        total = doubler(total)

        return total + tag.length
    }

    private fun compute(value: Int): Int {
        return value * 2
    }

    private fun emit(value: Int) {
        println(value)
    }
}
