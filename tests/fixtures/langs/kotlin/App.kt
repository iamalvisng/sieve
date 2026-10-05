typealias GreeterFn = () -> String

annotation class Marker

interface Greeter {
	fun greet(): String
}

data class Point(val x: Int, val y: Int)

class App(private val name: String) : Greeter {
	val label: String = "app"

	constructor() : this("default")

	companion object {
		fun create(name: String): App = App(name)
	}

	override fun greet(): String {
		return "hello ${this.name}, ${super.greet()}"
	}
}

object Registry {
	val apps = mutableListOf<App>()
}

val version: String = "1.0"

class FamAWidget

fun main() {
	val app = App("sieve")
	println(app.greet())
	Registry.apps.add(app)
}
