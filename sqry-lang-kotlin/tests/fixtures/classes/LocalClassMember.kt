class LocalClassMember {
    fun test() {
        val name = "outer"

        class Inner {
            val name = "inner"
            fun greet(): String {
                return name
            }
        }

        println(name)
        println(Inner().greet())
    }
}
