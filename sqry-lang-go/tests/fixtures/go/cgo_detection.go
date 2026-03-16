package main

/*
#include <stdlib.h>
*/
import "C"

import "unsafe"

func allocateMemory(size int) unsafe.Pointer {
	return C.malloc(C.size_t(size))
}

func freeMemory(ptr unsafe.Pointer) {
	C.free(ptr)
}

func main() {
	ptr := allocateMemory(1024)
	freeMemory(ptr)
}
