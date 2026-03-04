class WhenSubject {
    fun test(input: Any) {
        when (val x = input.toString()) {
            "hello" -> println(x)
            "world" -> println(x.length)
        }
    }
}
