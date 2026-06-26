// Hand-written Go sample exercising real control-flow kinds the body-shape
// descriptor buckets. MIT-clean, no vendored sources.

package sample

import (
	"errors"
	"fmt"
)

func classify(values []int, threshold int) (int, error) {
	total := 0
	for _, value := range values {
		if value > threshold {
			total += value
		} else if value < 0 {
			continue
		} else {
			break
		}
	}
	defer fmt.Println("done")
	switch total {
	case 0:
		return 0, nil
	default:
		if total < 0 {
			return 0, errors.New("classify failed")
		}
	}
	doubler := func(n int) int { return n * 2 }
	return doubler(total), nil
}

func feed(ch chan int, values []int) {
	for _, v := range values {
		ch <- v
	}
	go close(ch)
}
