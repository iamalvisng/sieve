pub trait Shape {
	fn area(&self) -> f64;
	fn name(&self) -> String {
		"shape".to_string()
	}
}

pub enum Color {
	Red,
	Green,
}

pub struct Circle {
	pub radius: f64,
}

impl Circle {
	pub fn new(radius: f64) -> Circle {
		Circle { radius }
	}

	pub fn scaled(&self, by: f64) -> Circle {
		Circle::new(self.radius * by)
	}
}

impl Shape for Circle {
	fn area(&self) -> f64 {
		3.14 * self.radius * self.radius
	}
}

macro_rules! square {
	($x:expr) => {
		$x * $x
	};
}

pub mod outer {
	pub mod inner {
		pub fn deep() -> i32 {
			shallow() + 1
		}
	}

	pub fn shallow() -> i32 {
		0
	}
}

pub type Pair = (i32, i32);

pub fn total() -> f64 {
	let c = Circle::new(2.0);
	c.area() + square!(2.0)
}
