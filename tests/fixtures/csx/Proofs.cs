namespace Csx;

public class Widget
{
    public void Draw() { }
    public static void StaticUtil() { }
    public virtual void OpenFree() { }
}

public sealed class Gadget : Widget
{
    public override void OpenFree() { }

    // CS10: a sealed class needs no subtype walk.
    public void GadgetCaller() { OpenFree(); }
}

public class BaseOne
{
    public void BaseHelper() { }
    protected void ProtHelper() { }
    private void PrivHelper() { }
    public virtual void VirtUnchanged() { }
    public virtual void VirtOverriddenBelow() { }
}

public class SubOne : BaseOne
{
    public override void VirtOverriddenBelow() { }

    // CS5: a bare call reaches an inherited method. An override replaces the
    // base method of the same signature.
    public void CallsBare()
    {
        BaseHelper();
        ProtHelper();
        VirtUnchanged();
        VirtOverriddenBelow();
    }
}

public struct Point
{
    public void Move() { }

    // CS10: a struct has no subtype.
    public void StructCaller() { Move(); }
}

public class Nesting
{
    public void OuterMethod() { }

    public class Inner
    {
        // CS5: the walk goes out to the enclosing class.
        public void InnerCaller() { OuterMethod(); }
    }
}

public class Receivers(Widget ctorParam)
{
    public void Local() { }
    public void Run(System.Action<Widget> a) { }

    public void ViaCtorParam() { ctorParam.Draw(); }
    public void ViaThis() { this.Local(); }
    public void ViaParam(Widget p) { p.Draw(); }
    public void ViaLocal() { Widget w = new Widget(); w.Draw(); }
    public void ViaVar() { var v = new Widget(); v.Draw(); }
    public void ViaForeach(Widget[] xs) { foreach (Widget f in xs) { f.Draw(); } }
    public void ViaLambda() { Run((Widget l) => l.Draw()); }
    public void ViaUsing() { using var u = new Widget(); u.Draw(); }
    public void ViaCatch() { try { } catch (System.Exception e) { } Local(); }
}

public class GBase<T>
{
    public void GenericBaseMethod() { }
}

public class GSub : GBase<int>
{
    // CS3: a generic base class resolves by its simple name.
    public void GenericCaller() { GenericBaseMethod(); }
}

public record Rec(int X)
{
    public void RecFirst() { RecSecond(); }
    public void RecSecond() { }
}

public class WithIface : System.IDisposable
{
    public void Dispose() { }

    // CS3: an interface outside the repo adds no class member.
    public void IfaceCaller() { IfaceTarget(); }
    public void IfaceTarget() { }
}

public class Widget2
{
    public void Shadowed(string s) { }

    // CS5: a bare call is no extension call, so the method stays proven.
    public void BareShadowed() { Shadowed("x"); }
}

public class FrozenName
{
    public int Where(int a) { return a; }

    // CS5: a bare call to a method named like an extension stays proven.
    public void BareWhere() { Where(1); }
}
