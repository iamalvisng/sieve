package p;

import java.util.List;
import q.PkgBase;
import q.Widget;

// B1: the union over the chain has 2 methods named m.
class B1Base {
    void m(String s) {}
}

class B1Sub extends B1Base {
    void m(Object o) {}

    void f() {
        m("a");
    }
}

// B2: a private member is not inherited, so the call goes to the outer class.
class B2Base {
    private void m() {}
}

class B2Outer {
    void m() {}

    class Inner extends B2Base {
        void f() {
            m();
        }
    }
}

// B2: a package-private member of another package is not inherited.
class PkgOuter {
    void hidden() {}

    class In extends PkgBase {
        void f() {
            hidden();
        }
    }
}

// B2: an interface static method is not inherited.
interface IStat {
    static void s() {}
}

class IOuter {
    static void s() {}

    class In implements IStat {
        void f() {
            s();
        }
    }
}

// B3: a record accessor is an implicit member.
class B3Outer {
    int x() {
        return 0;
    }

    record R(int x) {
        void f() {
            x();
        }
    }
}

// B4: an anonymous class, a local class and an enum constant body override.
class B4Foo {
    void m() {}
}

class B4Use {
    B4Foo make() {
        return new B4Foo() {
            @Override
            void m() {}
        };
    }

    void f() {
        B4Foo x = make();
        x.m();
    }
}

class B4Foo2 {
    void m() {}
}

class B4Local {
    void g() {
        class L extends B4Foo2 {
            @Override
            void m() {}
        }
    }

    void f(B4Foo2 x) {
        x.m();
    }
}

enum B4Enum {
    A {
        @Override
        void run() {}
    };

    void run() {}
}

class B4EnumUse {
    void f(B4Enum e) {
        e.run();
    }
}

// B5: an inherited field hides the field of the outer class.
class B5Bar {
    void m() {}
}

class B5Foo {
    void m() {}
}

class B5Base {
    B5Bar x;
}

class B5Outer {
    B5Foo x;

    class Inner extends B5Base {
        void f() {
            x.m();
        }
    }
}

// JV10: a subtype overrides the target.
class Animal {
    void speak() {}
}

class Dog extends Animal {
    @Override
    void speak() {}
}

class Zoo {
    void f(Animal a) {
        a.speak();
    }
}

// JV10: an abstract target and an interface target.
abstract class Shape {
    abstract void area();

    void g() {
        area();
    }
}

interface Named {
    String name();
}

class NamedUse {
    void f(Named n) {
        n.name();
    }
}

// JV1: 2 binders of one name, a pattern variable and a type parameter.
class Binders {
    void two(List<Widget> a, List<Widget> b) {
        for (Widget w : a) {
            w.draw();
        }
        for (Widget w : b) {
            w.draw();
        }
    }

    void pattern(Object o) {
        if (o instanceof Widget w) {
            w.draw();
        }
    }

    <T extends Widget> void typeParam(T t) {
        t.draw();
    }
}

// JV3: a supertype outside the repo makes the chain unknown.
class ChainJdk extends java.util.ArrayList<String> {
    void g() {
        helper();
    }

    void helper() {}
}

// JV9: a qualified type gives no edge.
class Qualified {
    Object f() {
        return new q.Widget();
    }
}
