public class Widget
{
    public void Draw() { }
    public void Shadowed(string s) { }
    public void Plain() { }
    public virtual void Open() { }

    // B4: a lost override in the text of a broken file stops this one.
    public void CallOpen() { Open(); }
    public void CallPlain() { Plain(); }
}
