namespace Csx.Members;

public interface IPart
{
    void Probe();
}

public class Engine
{
    public void Start() { }
    public static void Boot() { }
    public static void Over(int a) { }
    public static void Over(string a) { }
}

public static class EngineExt
{
    public static int Twice(this Engine e, int n) { return n; }
}

public class Root
{
    public virtual void Hook() { }
}

public class Mid : Root
{
    public override void Hook() { }
}

public class Leaf : Mid
{
    // CS6: the nearest member up the chain is the override in Mid.
    public void CallBase() { base.Hook(); }
}

public class Leaf2 : Root
{
    // CS6: the call is non-virtual, so a subtype override does not matter.
    public void CallBaseRoot() { base.Hook(); }
}

public abstract class AbsRoot
{
    public abstract void Must();
}

public class Impl : AbsRoot
{
    public override void Must() { base.Must(); }
    public string CallObj() { return base.ToString(); }
}

public class Users
{
    // CS7: a static method of a type, and a static extension call.
    public void CallBoot() { Engine.Boot(); }
    public int CallTwice(Engine e) { return EngineExt.Twice(e, 1); }
    // CS7 stops: an instance method, an overload, a local of the name.
    public void CallInstance() { Engine.Start(); }
    public void CallOver() { Engine.Over(1); }
    public void CallLocal() { Engine Engine = new Engine(); Engine.Boot(); }
}

public class ColorColor
{
    private Engine Engine = new Engine();

    // Color Color: the field wins, and the static method then stops.
    public void CallColor() { Engine.Boot(); }
}

public class Car
{
    private Engine eng = new Engine();
    private IPart part = null;
    public Engine Prop { get; set; }

    // CS8: a field receiver with or without `this.`.
    public void CallField() { eng.Start(); }
    public void CallThisField() { this.eng.Start(); }
    // Stops: a property, an interface field, a hiding local.
    public void CallProp() { Prop.Start(); }
    public void CallPart() { part.Probe(); }
    public void CallHidden(Engine eng) { eng.Start(); }

    public class Nested
    {
        // A field of the outer class is no field of the nested class.
        public void CallOuter(Car c) { c.eng.Start(); }
    }
}
