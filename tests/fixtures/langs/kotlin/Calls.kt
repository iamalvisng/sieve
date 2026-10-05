class Box {
	fun open(): Int = 1

	fun reopen(): Int {
		return this.open()
	}

	companion object {
		fun make(): Box = Box()
	}
}

fun String.shout(): String = this

fun helper(): Int = 2

fun each(items: List<Int>) {
	items.forEach { println(helper()) }
	run { helper() }
	Box.make()
	"hi".shout()
}
