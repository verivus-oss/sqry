library(stats)

add <- function(a, b) {
  return(a + b)
}

multiply <- function(a, b) {
  a * b
}

Calculator <- R6::R6Class("Calculator",
  public = list(
    value = 0,
    initialize = function() {
      self$value <- 0
    },
    add = function(x) {
      self$value <- self$value + x
      invisible(self)
    }
  )
)

setClass("Point",
  slots = c(x = "numeric", y = "numeric")
)
