package com.example

data class User(val name: String, val age: Int)

sealed class Result {
    data class Success(val value: String) : Result()
    data class Error(val message: String) : Result()
}

suspend fun fetchData(): String {
    return "data"
}

fun main() {
    val user = User("Alice", 30)
    println(user)
}
