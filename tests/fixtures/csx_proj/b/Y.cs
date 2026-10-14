public class Y
{
    void Own() { }
    void ViaThis() { this.Own(); }
    void BareOwn() { Own(); }
    void Cross(Lib l) { l.Pub(); }
}
