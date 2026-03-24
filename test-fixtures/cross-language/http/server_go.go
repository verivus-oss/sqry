package main

import (
	"fmt"
	"net/http"
)

func handleUsers(w http.ResponseWriter, r *http.Request) {
	fmt.Fprintf(w, "users list")
}

func handleItems(w http.ResponseWriter, r *http.Request) {
	fmt.Fprintf(w, "items list")
}

func main() {
	http.HandleFunc("/api/users", handleUsers)
	http.HandleFunc("/api/items", handleItems)
	http.ListenAndServe(":8080", nil)
}
