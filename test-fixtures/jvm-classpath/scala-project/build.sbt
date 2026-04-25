name := "example-scala"
version := "1.0.0"
scalaVersion := "3.4.0"

libraryDependencies ++= Seq(
  "com.typesafe.akka" %% "akka-actor-typed" % "2.9.0",
  "org.scalatest" %% "scalatest" % "3.2.17" % Test
)
