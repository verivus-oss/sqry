package demo.app

import demo.app.Extensions.trimAll
import demo.lib.Repository as RepoLib
import demo.util.*

class Deferred<T>(val value: T)

object RepoLib {
    suspend fun fetch(value: Int): Deferred<Int> = Deferred(value)
}

object Extensions {
    fun String.trimAll(): String = this.trim()
}

class Service {
    suspend fun process(value: Int): Deferred<Int> {
        val result = RepoLib.fetch(value)
        return result
    }

    fun format(name: String): String {
        return name.trimAll()
    }
}
