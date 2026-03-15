class NamedArguments {
    fun greet(name: String, greeting: String): String {
        return "$greeting, $name"
    }

    fun test() {
        val name = "world"
        val result = greet(name = name, greeting = "hello")
        println(result)
    }
}
