using System;

namespace Sieve {

interface IGreeter {
	string Greet();
}

class App : IGreeter {
	private string name;

	public App(string name) {
		this.name = name;
	}

	public string Greet() {
		return "hello " + name;
	}

	static void Main(string[] args) {
		var app = new App("sieve");
		Console.WriteLine(app.Greet());
	}
}

}
