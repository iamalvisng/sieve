package p;

// F1: a sub-interface of a subtype overrides the default method.
interface F1I {
    default void m() {}
}

class F1Recv implements F1I {}

interface F1J extends F1I {
    default void m() {}
}

class F1Sub2 extends F1Recv implements F1J {}

class F1Use {
    void f(F1Recv r) {
        r.m();
    }
}

// F2: a class type parameter named like a java.lang type.
class F2Base {
    void m(String s) {}
}

class F2Sub<String> extends F2Base {
    void m(String s) {}

    void f() {
        m(null);
    }
}

// F4: a covariant return is an override. A generic parameter is not
// comparable by name, so the call stops.
class F4Base {
    Object get() {
        return null;
    }
}

class F4Sub extends F4Base {
    String get() {
        return "";
    }

    Object f() {
        return get();
    }
}

class F4Gen<T> {
    void put(T t) {}
}

class F4GenSub extends F4Gen<String> {
    void put(String s) {}

    void g() {
        put("");
    }
}

class F4Use {
    void h(F4Gen<String> g) {
        g.put("");
    }
}
