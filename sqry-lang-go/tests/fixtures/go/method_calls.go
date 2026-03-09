package main

type Counter struct {
	value int
}

func (c *Counter) Increment() {
	c.value++
}

func (c *Counter) GetValue() int {
	return c.value
}

func main() {
	counter := Counter{value: 0}
	counter.Increment()
	val := counter.GetValue()
}
