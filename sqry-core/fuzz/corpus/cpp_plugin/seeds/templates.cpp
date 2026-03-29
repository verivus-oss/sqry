template<typename T, int N>
struct Array {
    T data[N];

    template<typename U>
    U get(int i) const {
        return static_cast<U>(data[i]);
    }
};

template<template<typename> class Container, typename T>
class Wrapper {
    Container<T> inner;
};

// Variadic templates
template<typename... Args>
void func(Args... args) {}

// SFINAE
template<typename T>
typename std::enable_if<std::is_integral<T>::value, T>::type
process(T value) {
    return value;
}
