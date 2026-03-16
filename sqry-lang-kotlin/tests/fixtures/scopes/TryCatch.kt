class TryCatch {
    fun test() {
        try {
            val result = compute()
            println(result)
        } catch (e: Exception) {
            println(e)
        }
    }

    fun compute(): Int = 42
}
