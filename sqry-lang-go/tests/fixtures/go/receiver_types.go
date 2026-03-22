package main

type Point struct {
	x, y float64
}

// Value receiver
func (p Point) Distance() float64 {
	return p.x * p.x + p.y * p.y
}

// Pointer receiver
func (p *Point) Move(dx, dy float64) {
	p.x += dx
	p.y += dy
}

func main() {
	point := Point{x: 1.0, y: 2.0}
	point.Distance()
	point.Move(3.0, 4.0)
}
