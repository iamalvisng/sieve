import java.util.List;
import util.Helper;

interface Greeter {
	String greet();
}

enum Level {
	LOW, HIGH
}

record Point(int x, int y) {
}

class App implements Greeter {
	private String name;

	App(String name) {
		this.name = name;
	}

	public String greet() {
		return "hello " + name;
	}

	public String join(String... parts) {
		return List.of(parts).toString();
	}

	public static void main(String[] args) {
		App app = new App("sieve");
		System.out.println(app.greet());
	}

	public static void famACall() {
		new FamAWidget();
	}

	public static void famCCall() {
		new FamCThing();
	}
}
