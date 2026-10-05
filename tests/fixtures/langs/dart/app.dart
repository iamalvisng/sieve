import "dart:core";

class App {
	String name;

	App(this.name);

	String greet() {
		return "hello " + name;
	}
}

String run() {
	final app = App("sieve");
	return app.greet();
}
