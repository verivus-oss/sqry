class LambdaExplicit {
    fun test() {
        val items = listOf(1, 2, 3)
        val result = items.map { x -> x + 1 }
        println(result)
    }
}
