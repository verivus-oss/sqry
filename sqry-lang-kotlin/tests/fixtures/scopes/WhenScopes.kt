class WhenScopes {
    fun test(value: Any) {
        when (value) {
            is String -> {
                val len = value.toString()
                println(len)
            }
            is Int -> {
                val doubled = value.toString()
                println(doubled)
            }
        }
    }
}
