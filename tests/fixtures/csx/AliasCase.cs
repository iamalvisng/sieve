using Gizmo = Csx.Other.Gizmo;

namespace Csx.Alias;

// CS2: an alias of the name stops the lookup.
public class AliasUse
{
    public void NewAlias() { var g = new Gizmo(); }
}
