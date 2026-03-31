package main

import "fmt"

type Point struct {
    X int
    Y int
}

func (p *Point) Move(dx, dy int) {
    p.X += dx
    p.Y += dy
}

func Add(a, b int) int {
    return a + b
}

func main() {
    p := &Point{X: 0, Y: 0}
    p.Move(5, 10)
    fmt.Println(p)
}
