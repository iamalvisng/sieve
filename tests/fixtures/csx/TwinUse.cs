// CS2: 2 usings that bring the same type name stop.
using TwinA;
using TwinB;

namespace Csx.Names;

public class TwinUse
{
    public void NewTwin() { var t = new Twin(); }
}
