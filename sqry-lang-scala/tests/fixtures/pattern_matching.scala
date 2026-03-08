package com.example

sealed trait Shape
case class Circle(radius: Double) extends Shape
case class Rectangle(width: Double, height: Double) extends Shape

object ShapeCalculator {
  def area(shape: Shape): Double = shape match {
    case Circle(r) => Math.PI * r * r
    case Rectangle(w, h) => w * h
  }

  def describe(shape: Shape): String = shape match {
    case Circle(r) => s"Circle with radius $r"
    case Rectangle(w, h) => s"Rectangle ${w}x${h}"
  }
}
