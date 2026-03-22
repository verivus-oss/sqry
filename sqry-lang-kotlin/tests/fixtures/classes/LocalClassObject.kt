class LocalClassObject {
    fun test() {
        val outer = 10
        class Inner {
            fun use() {
                println(outer)
            }
        }
        val inner = Inner()
        println(inner)
    }
}
