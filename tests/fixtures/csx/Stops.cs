namespace Csx.Stops;

// CS4: 2 methods of one name stop.
public class OverloadHost
{
    public void Ov(int a) { }
    public void Ov(string a) { }
    public void CallOv() { Ov(1); }
}

// CS4: a field of the name stops.
public class FieldBase
{
    public void Hid() { }
}

public class FieldSub : FieldBase
{
    public new System.Action Hid;
    public void CallHid() { Hid(); }
}

// CS4: an optional parameter, a `params` parameter or a wrong count stops.
public class LooseHost
{
    public void Opt(int a = 1) { }
    public void Par(params int[] xs) { }
    public void Arity(int a) { }
    public void CallOpt() { Opt(); }
    public void CallPar() { Par(1, 2); }
    public void CallArity() { Arity(1, 2); }
}

// B5: a private base method is no candidate.
public class PrivBase
{
    private void PrivOnly() { }
}

public class PrivSub : PrivBase
{
    public void CallPrivOnly() { PrivOnly(); }
}

// CS10: an abstract target is never exact.
public abstract class AbsHost
{
    public abstract void Absent();
    public void CallAbsent() { Absent(); }
}

// CS10: a subtype overrides the target.
public class VirtHost
{
    public virtual void Over() { }
    public void CallOver() { Over(); }
}

public class VirtSub : VirtHost
{
    public override void Over() { }
}

// CS10: the override sits 2 levels below.
public class DeepHost
{
    public virtual void DeepOver() { }
    public void CallDeepOver() { DeepOver(); }
}

public class DeepMid : DeepHost { }

public class DeepLeaf : DeepMid
{
    public override void DeepOver() { }
}

// CS8: an interface receiver.
public interface IRun
{
    void Go();
}

public class IfaceUse
{
    public void CallGo(IRun r) { r.Go(); }
}

// CS1: a binder that has no exact scope, a type parameter, a local function.
public class Binders
{
    public void Shadow(int a) { }

    public void LocalFn()
    {
        int Shadow(int a) => a;
        Shadow(1);
    }

    public void PatternVar(object o)
    {
        if (o is Csx.Widget pw) { pw.Draw(); }
    }

    public void OutVar()
    {
        Fill(out var ov);
        ov.Draw();
    }

    public void Deconstructed((Csx.Widget, int) pair)
    {
        var (dw, dn) = pair;
        dw.Draw();
    }

    public void Fill(out Csx.Widget w) { w = null; }
    public void TypeParam<Widget>(Widget p) { p.Draw(); }
}

// CS0: a shape that this slice leaves out gives no Extracted edge.
public class Leftovers
{
    private Csx.Widget fld;

    public void ViaField() { fld.Draw(); }
    public void ViaStatic() { Csx.Widget.StaticUtil(); }
    public void ViaChain() { Make().Draw(); }
    public Csx.Widget Make() { return null; }
    public void ViaNullCheck(Csx.Widget w) { w?.Draw(); }
}

public class BaseCaller : BaseOne
{
    public void ViaBase() { base.BaseHelper(); }
}

// CS9: a struct, a partial class, an array, a qualified name and a delegate.
public struct StructNew { }

public partial class PartNew { }

public delegate void DelNew();

public class NewStops
{
    public void NewStruct() { var s = new StructNew(); }
    public void NewPartial() { var p = new PartNew(); }
    public void NewArray() { var a = new Csx.Widget[3]; }
    public void NewQualified() { var q = new Csx.Widget(); }
    public void NewTargetTyped() { Csx.Widget t = new(); }
    public void NewDelegate() { var d = new DelNew(NewDelegate); }
}

// CS8: an extension method of the name in the repo stops a member call.
public class ExtHost
{
    public void Shadowed(string s) { }
}

public static class ExtHolder
{
    public static void Shadowed(this ExtHost h, int i) { }
}

public class ExtUse
{
    public void CallShadowed(ExtHost h) { h.Shadowed("x"); }
}

// CS8: a frozen extension name stops a member call.
public class FrozenUse
{
    public int Where(int a) { return a; }
    public void CallWhere() { this.Where(1); }
}

// CS3: a base class outside the repo makes the chain unknown.
public class OutsideBase : System.ComponentModel.Component
{
    public void CallInside() { Inside(); }
    public void Inside() { }
}
