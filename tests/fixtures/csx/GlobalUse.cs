namespace Elsewhere;

// Owner decision: a type found only through a `global using` of another file
// stops.
public class GlobalUse
{
    public void NewGlobal() { var g = new Gizmo(); }
}
