// H2: a type and a generic type of one name stop.
namespace Csx.Arity;

public class Both { }

public class Both<T> { }

public class ArityUse
{
    public void NewBoth() { var b = new Both<int>(); }
}
