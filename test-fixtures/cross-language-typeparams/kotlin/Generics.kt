package crosslanguage.generics

interface A
interface B

class KBox<out T>(val value: T)

inline fun <reified T> ktIdentity(value: T): T = value

fun <T> ktConstrained(value: T): T where T : A, T : B = value

class KStore<T> where T : A {
    fun <U : B> put(value: U): U = value
}
