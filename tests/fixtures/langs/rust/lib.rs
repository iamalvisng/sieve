use std::fmt;

use crate::util::helper;
use crate::missing::nope;

pub const MAX_NAME_LEN: usize = 32;
pub static DEFAULT_NAME: &str = "sieve";

pub enum Level {
	Low,
	High,
}

pub trait Greeter {
	fn greet(&self) -> String;
}

pub struct App {
	pub name: String,
}

impl Greeter for App {
	fn greet(&self) -> String {
		format!("hello {}", self.name)
	}
}

impl fmt::Display for App {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}", self.name)
	}
}

pub mod util {
	pub fn run(app: &super::App) -> String {
		app.greet()
	}
}
