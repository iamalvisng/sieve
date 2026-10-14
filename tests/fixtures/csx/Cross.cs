using Csx.Other;

namespace Csx.App;

public class CrossUse
{
    // CS2: a type found through a `using` of the file.
    public void ViaUsing() { var g = new Gizmo(); g.Spin(); }

    // CS2: an enclosing namespace comes before a `using`.
    public void ViaEnclosing() { var w = new Widget(); w.Draw(); }

    // CS2: `Csx.Dup` is in an enclosing namespace, so it beats `Csx.Other.Dup`.
    public void ViaOrder() { var d = new Dup(); d.WhichCsx(); }
}
