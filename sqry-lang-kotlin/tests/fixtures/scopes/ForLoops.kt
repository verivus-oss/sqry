class ForLoops {
    fun testBasicFor() {
        val items = listOf(1, 2, 3)
        for (item in items) {
            println(item)
        }
    }

    fun testDestructuringFor() {
        val map = mapOf("a" to 1, "b" to 2)
        for ((key, value) in map) {
            println(key)
            println(value)
        }
    }
}
