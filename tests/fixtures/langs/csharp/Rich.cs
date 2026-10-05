namespace Rich {

interface IShape {
	double Area();
}

enum Color {
	Red,
	Green
}

record Person(string Name, int Age);

class Circle : IShape {
	public double Radius { get; set; }

	public Circle(double radius) {
		Radius = radius;
	}

	public double Area() {
		return 3.14 * Radius * Radius;
	}

	public static double Total() {
		var c = new Circle(2.0);
		return c.Area() + Helper();
	}

	static double Helper() {
		return 1.0;
	}
}

}
