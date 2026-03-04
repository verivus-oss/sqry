class LocalClassInterface {
    fun process() {
        val captured = "hello"
        val length = captured.length

        class Printer : Runnable {
            override fun run() {
                println(captured)
                println(length)
            }
        }

        Printer().run()
    }
}
