enum class Color { RED, GREEN }

enum class Level(val rank: Int) {
	LOW(1),
	HIGH(2);

	fun next(): Level = HIGH
}

enum class Op {
	ADD {
		override fun apply(a: Int, b: Int): Int = a + b
	},
	SUB {
		override fun apply(a: Int, b: Int): Int = a - b
	};

	abstract fun apply(a: Int, b: Int): Int
}
