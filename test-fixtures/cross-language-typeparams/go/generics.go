package main

type Number interface {
    ~int | ~float64
}

type List[E any] struct {
    items []E
}

func Map[T any, U comparable](xs []T, f func(T) U) []U {
    return nil
}

func Sum[T int | float64](values []T) T {
    var zero T
    return zero
}

func (l *List[E]) Push(v E) {
    l.items = append(l.items, v)
}
