import Foundation

protocol Greeter {
	func greet() -> String
}

typealias Name = String

struct Point {
	var x: Int
	var y: Int
}

class App: Greeter {
	var name: Name

	init(name: Name) {
		self.name = name
	}

	func greet() -> String {
		return "hello \(name)"
	}
}

func run() {
	let app = App(name: "sieve")
	print(app.greet())
}
