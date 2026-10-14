public class Use
{
    // B4: only the lost extension name stops a member call.
    public void ViaDraw(Widget w) { w.Draw(); }
    public void ViaShadowed(Widget w) { w.Shadowed("x"); }
    public void ViaNew() { var w = new Widget(); }
}

// B4: a type of a broken file is unknown, so its subtype stops.
public class Child : Zed
{
    public void ChildCaller() { ChildTarget(); }
    public void ChildTarget() { }
}
