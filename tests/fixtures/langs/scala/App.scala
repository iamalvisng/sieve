trait Greeter {
	def greet(): String
}

class App(name: String) extends Greeter {
	def greet(): String = "hello " + name
}

object App {
	def run(): String = {
		val app = new App("sieve")
		app.greet()
	}
}
