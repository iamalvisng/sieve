// H3: a conditional twin of a type stops.
namespace Csx.Cond;

#if NET8_0
public class CondTwin { }
#else
public class CondTwin { }
#endif

public class CondUse
{
    public void NewCond() { var c = new CondTwin(); }
}
