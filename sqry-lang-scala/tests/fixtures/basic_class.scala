package com.example

class UserService(db: Database) {
  def findById(id: Long): Option[User] = {
    db.query(s"SELECT * FROM users WHERE id = $id")
      .headOption
      .map(User.fromRow)
  }

  def createUser(name: String, email: String): User = {
    val user = User(name = name, email = email)
    db.insert("users", user.toRow)
    user
  }
}

trait Repository[T] {
  def findAll(): Seq[T]
  def save(entity: T): T
}

case class User(name: String, email: String)
