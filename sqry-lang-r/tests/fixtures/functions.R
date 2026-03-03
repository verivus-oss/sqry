#' @export
add <- function(x, y = 1) {
  x + y
}

print.myclass <- function(x, ...) {
  x
}

setMethod("show", "MyClass", function(object) {
  print(object)
})

MyClass <- R6Class("MyClass", public = list(
  initialize = function(value) {
    self$value <- value
  }
))
