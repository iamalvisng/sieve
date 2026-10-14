public class Z
{
    void Own() { }
    void ViaThis() { this.Own(); }
    void CrossPub(Lib l) { l.Pub(); }
    void CrossInternal(Lib l) { l.Imp(); }
}
