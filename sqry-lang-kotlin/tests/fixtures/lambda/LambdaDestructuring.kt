class LambdaDestructuring {
    fun test() {
        val pairs = listOf(Pair(1, "a"), Pair(2, "b"))
        pairs.forEach { (num, text) ->
            println(num)
            println(text)
        }
    }
}
