// B3: a `using` inside a namespace block stops the type lookups of the file.
namespace Csx.InnerUsing
{
    using Csx.Other;

    public class InnerUse
    {
        public void NewGizmo() { var g = new Gizmo(); }
        public void NewWidget() { var w = new Csx.Widget(); }
    }
}
