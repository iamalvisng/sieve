package fx

/** KDoc in front of a class. The declaration start skips it. */
@Suppress("unused")
open class Base {
    open fun greet(s: String): String = s
    fun plain() = 1
}

interface Shape {
    fun area(): Int
    fun name(): String = "shape"
}

class Sq(val n: Int) : Shape {
    constructor() : this(1)
    override fun area(): Int = n * n
}

class Sub : Base() {
    override fun greet(s: String): String = s + "!"
}

fun top(x: Int) = x

fun String.shout() = this + "!"

class Use {
    fun run(b: Base, sh: Shape, f: (Int) -> Int) {
        b.greet("a")
        b.plain()
        sh.area()
        sh.name()
        top(1)
        "x".shout()
        f(2)
        Sq(3)
        Sq()
        listOf(1).map { top(it) }
        fun inner() = 1
        inner()
        val a = 1 + 2
        println(a)
    }
}

enum class Color { RED, GREEN }

object Reg {
    val size = 0
}

class Holder {
    companion object {
        fun make() = Holder()
    }
}
