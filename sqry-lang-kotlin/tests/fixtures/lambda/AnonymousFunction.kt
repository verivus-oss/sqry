class AnonymousFunction {
    fun test() {
        val transform = fun(x: Int): Int {
            return x * 2
        }
        val result = transform(5)
        println(result)
    }
}
