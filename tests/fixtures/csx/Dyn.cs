namespace Csx.Dyn;

// A file that names `dynamic` can bind a call at run time, so no call is
// proven.
public class DynUse
{
    public dynamic D;
    public void DynHelper() { }
    public void CallDynHelper() { DynHelper(); }
}
