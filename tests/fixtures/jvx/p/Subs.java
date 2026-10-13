package p;

class Sub extends Base {
    void inherited() {
        baseOnly();
    }

    // B-a: a private member type of the super class is not inherited.
    void viaNode(Node n) {
        n.go();
    }
}

class SuperCaller extends Base {
    void viaSuper() {
        super.sup();
    }
}

interface Greeter {
    String greet();
}

class Impl implements Greeter {
    public String greet() {
        return "";
    }

    String twice() {
        return greet() + greet();
    }
}
