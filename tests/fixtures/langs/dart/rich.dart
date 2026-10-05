mixin Walker {
	void walk() {
		print("walk");
	}
}

extension StringExt on String {
	String shout() {
		return toUpperCase();
	}
}

enum Color { red, green }

class Animal {
	String name;

	Animal(this.name);

	String speak() {
		return "hi " + name;
	}

	void act() {
		speak();
	}
}

String describe(Animal a) {
	return a.speak();
}

void main() {
	final a = Animal("x");
	describe(a);
}
