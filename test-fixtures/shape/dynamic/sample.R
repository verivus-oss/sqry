# Hand-written R control-flow sample for shape descriptor coverage.

classify <- function(value, label = "n/a", ...) {
  result <- 0
  if (value > 100) {
    result <- 1
  } else if (value > 10) {
    result <- 2
  } else {
    result <- 3
  }

  while (result > 0) {
    result <- result - 1
    if (result == 2) {
      next
    }
    if (result < 0) {
      break
    }
  }

  for (i in seq_len(3)) {
    helper(i)
  }

  rest <- list(...)
  doubled <- sapply(rest, function(x) x * 2)

  squared <- vapply(rest, function(y) y * y, numeric(1))

  result <- tryCatch(
    {
      if (value < 0) stop("bad")
      helper(value)
    },
    error = function(e) -1
  )

  return(result)
}

helper <- function(x) {
  x + 1
}
