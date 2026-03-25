def foo() {
  println("foo")
  return {b -> b()}
}
def bar() {
  println("bar")
  return {println("hello")}
}
foo() bar()
