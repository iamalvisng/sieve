namespace Csx.Parts;

public partial class PartHost
{
    // CS2: partial parts merge into one type.
    public void FromA() { FromB(); }
}

public partial class PartOuter
{
    public void OuterM() { }

    // H1: a partial type without the name stops the walk.
    public partial class PartInner
    {
        public void InnerF() { OuterM(); }
    }
}

public partial class PartMethod
{
    // CS4: a partial method has 2 declarations.
    partial void Hook();
    partial void Hook() { }
    public void CallHook() { Hook(); }
}
