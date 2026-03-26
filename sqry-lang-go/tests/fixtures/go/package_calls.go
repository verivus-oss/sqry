package utils

import (
	"fmt"
	"strings"
)

func ProcessString(s string) string {
	upper := strings.ToUpper(s)
	fmt.Println(upper)
	return upper
}
