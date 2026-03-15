class NestedLambdaIt {
    fun test() {
        val items = listOf(1, 2, 3)
        items.forEach {
            val doubled = it * 2
            listOf("a").map { inner ->
                println(inner)
            }
        }
    }
}
