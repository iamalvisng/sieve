using Some.Package;

namespace Csx.Scopes;

// B2: a `using` of a namespace outside the repo and the frozen list stops a
// member call. A bare call stays proven.
public class UnknownUse
{
    public void Helper() { }
    public void CallHelperBare() { Helper(); }
    public void CallHelperThis() { this.Helper(); }
    public void CallDraw(Csx.Widget w) { w.Draw(); }
}
