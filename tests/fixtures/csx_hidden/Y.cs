public class Y
{
    void Own() { }
    void ViaThis() { this.Own(); }
    void BareOwn() { Own(); }
}
