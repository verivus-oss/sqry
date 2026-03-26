class FunctionTypedLocal {
    fun test() {
        val f = { x: Int -> x * 2 }
        val result = f(5)
        println(result)
    }
}
