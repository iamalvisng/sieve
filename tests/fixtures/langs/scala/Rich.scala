trait Shape {
	def area(): Double
}

case class Circle(radius: Double) extends Shape {
	def area(): Double = 3.14 * radius * radius
}

object Shapes {
	val unit: Circle = Circle(1.0)

	def total(): Double = {
		val c = Circle(2.0)
		helper() + c.area()
	}

	def helper(): Double = 1.0
}
