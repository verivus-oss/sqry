class NestedScopes {
    fun test() {
        val x = 1
        if (x > 0) {
            val x = 2
            println(x)
        }
        println(x)
    }
}
