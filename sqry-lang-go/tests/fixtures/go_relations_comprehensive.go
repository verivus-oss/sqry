// Comprehensive Go relations test fixture
// Covers: calls, imports, exports, method receivers, anonymous functions

package main

import (
	"fmt"
	"net/http"
	"os"

	"github.com/gin-gonic/gin"
	_ "github.com/lib/pq"
)

// Exported types
type Server struct {
	port int
}

type Config struct {
	Host string
	Port int
}

// unexported type
type internal struct{}

// Exported constants
const MaxRetries = 3
const DefaultTimeout = 30

// unexported constant
const minConnections = 1

// Exported variables
var GlobalConfig *Config
var DefaultServer *Server

// unexported variable
var localCache map[string]string

// Exported function
func NewServer(port int) *Server {
	return &Server{port: port}
}

// unexported function
func validatePort(port int) bool {
	return port > 0 && port < 65536
}

// Exported method with pointer receiver
func (s *Server) Start() error {
	if !validatePort(s.port) {
		return fmt.Errorf("invalid port")
	}
	s.initialize()
	s.listen()
	return nil
}

// Exported method with value receiver
func (s Server) GetPort() int {
	return s.port
}

// unexported method
func (s *Server) initialize() {
	fmt.Println("Initializing server...")
	s.loadConfig()
}

func (s *Server) loadConfig() {
	// Load configuration
}

// unexported method
func (s *Server) listen() {
	http.HandleFunc("/", s.handleRequest)
	http.ListenAndServe(fmt.Sprintf(":%d", s.port), nil)
}

func (s *Server) handleRequest(w http.ResponseWriter, r *http.Request) {
	fmt.Fprintf(w, "Hello from server")
}

// Function with anonymous function
func ProcessRequests(requests []string) {
	for _, req := range requests {
		func(r string) {
			fmt.Println(r)
			validateRequest(r)
		}(req)
	}
}

func validateRequest(req string) bool {
	return len(req) > 0
}

// Chained method calls
func SetupRouter() *gin.Engine {
	router := gin.Default()
	router.Use(gin.Logger()).Use(gin.Recovery())
	return router
}

// Multiple calls in one function
func Initialize() {
	LoadEnvironment()
	ConnectDatabase()
	StartMetrics()
}

func LoadEnvironment() {
	os.Getenv("PORT")
}

func ConnectDatabase() {
	// DB connection
}

func StartMetrics() {
	// Metrics
}

// Package-level function calls
func main() {
	server := NewServer(8080)
	server.Start()

	ProcessRequests([]string{"req1", "req2"})
	SetupRouter()
	Initialize()
}
