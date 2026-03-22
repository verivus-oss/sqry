class ParameterlessLambda {
    fun test() {
        val x = 10
        run { println(x) }

        val items = listOf(1, 2, 3)
        items.forEach { println(it) }
    }
}
