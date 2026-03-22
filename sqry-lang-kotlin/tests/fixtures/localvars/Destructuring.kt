class Destructuring {
    fun testPair() {
        val pair = Pair(1, "hello")
        val (num, text) = pair
        println(num)
        println(text)
    }
}
