package p;

import java.util.List;
import q.Util;
import q.Widget;
import r.*;

public class Use {
    private Widget field = new Widget();

    void helper() {}

    void bare() {
        helper();
    }

    void viaThis() {
        this.helper();
    }

    int stat() {
        return Util.twice(2);
    }

    void local() {
        Widget w = new Widget();
        w.draw();
    }

    void viaVar() {
        var v = new Widget();
        v.draw();
    }

    void viaField() {
        field.draw();
    }

    void viaThisField() {
        this.field.draw();
    }

    void viaParam(Widget p) {
        p.draw();
    }

    void viaLoop(List<Widget> xs) {
        for (Widget x : xs) {
            x.draw();
        }
    }

    void viaLambda() {
        java.util.function.Consumer<Widget> c = (Widget y) -> y.draw();
    }

    void viaTry() throws Exception {
        try (java.io.StringReader rd = new java.io.StringReader("")) {
            Widget z = new Widget();
            z.draw();
        } catch (RuntimeException e) {
            helper();
        }
    }

    Object onDemand() {
        return new Gadget();
    }

    void enumReceiver(Color c) {
        c.paint();
    }

    void member() {
        Item i = new Item();
        i.ping();
    }

    static class Item {
        void ping() {}
    }

    enum Color {
        RED;

        void paint() {}
    }
}
