// Oracle for C# call links. Usage:
//   dotnet CsCalls.dll <build.json> --root <repo> [-o rows.jsonl]
// build.json is the output of: dotnet build -f <tfm> -t:Build --artifacts-path <scratch>
//   -getItem:Compile -getItem:ReferencePathWithRefAssemblies -getProperty:DefineConstants,LangVersion,TargetFramework
// It compiles every Compile item in one CSharpCompilation. It writes one JSON row per
// invocation and object creation whose source file is under --root:
//   {src, line, shape, dst, name, dst_start, dst_end, flags...}
// Paths are relative to --root. Lines are 1-based. dst_start..dst_end is the declaration
// span of the target. A constructor maps to its type declaration, header only (up to the
// open brace). A partial target gets one extra row per more part, with alt:true.
// A target with no in-repo source (BCL, jar, generated file) gets oor:true and dst "";
// a target with no tree in the repo (delegate Invoke, implicit member) gets impl:true.
// Shapes: bare, this, base, static, var, field, new, chain, other.
// Flags (only when true): static, virtual, abs (abstract or interface member), override,
// sealed, iface (owner is an interface), ext (extension method), local (local function),
// deleg (delegate Invoke), dyn (dynamic dispatch), ovr (an in-repo subtype of the receiver's
// static type overrides or implements the target: the runtime target may differ), gen.
// Inactive #if text gets rows {inactive:true, src, start, end}; they are not calls.
// Error diagnostics go to stderr, then "cs errors: N". Exit 1 if N > 0.
using System.Collections.Immutable;
using System.Text.Json;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;

string input = null, root = null, outPath = null, genDir = null;
for (int i = 0; i < args.Length; i++)
{
    if (args[i] == "--root") root = args[++i];
    else if (args[i] == "-o") outPath = args[++i];
    else if (args[i] == "--gen") genDir = args[++i];
    else if (args[i] == "--lib") { }
    else input = args[i];
}
root = Path.GetFullPath(root).TrimEnd('/') + "/";
var doc = JsonDocument.Parse(File.ReadAllText(input)).RootElement;
var props = doc.GetProperty("Properties");
string P(string k) => props.TryGetProperty(k, out var v) ? v.GetString() : "";
var items = doc.GetProperty("Items");
var defines = P("DefineConstants").Split(new[] { ';', ',' }, StringSplitOptions.RemoveEmptyEntries);
var lang = P("LangVersion");
if (!LanguageVersionFacts.TryParse(string.IsNullOrEmpty(lang) ? "latest" : lang, out var lv)) lv = LanguageVersion.Latest;
var popts = new CSharpParseOptions(lv, DocumentationMode.None, SourceCodeKind.Regular, defines);
var trees = new List<SyntaxTree>();
foreach (var c in items.GetProperty("Compile").EnumerateArray())
{
    var p = c.GetProperty("FullPath").GetString();
    trees.Add(CSharpSyntaxTree.ParseText(File.ReadAllText(p), popts, p));
}
if (genDir != null && Directory.Exists(genDir))   // Source generator output; rows from these files are dropped
    foreach (var p in Directory.EnumerateFiles(genDir, "*.cs", SearchOption.AllDirectories).Where(p => p.Contains("/generated/")))
        trees.Add(CSharpSyntaxTree.ParseText(File.ReadAllText(p), popts, p));
var refs = items.GetProperty("ReferencePathWithRefAssemblies").EnumerateArray()
    .Select(r => (MetadataReference)MetadataReference.CreateFromFile(r.GetProperty("FullPath").GetString())).ToList();
// the project assembly name makes InternalsVisibleTo work; an Exe project needs the console kind
var comp = CSharpCompilation.Create(string.IsNullOrEmpty(P("AssemblyName")) ? "oracle" : P("AssemblyName"), trees, refs,
    new CSharpCompilationOptions(P("OutputType") is "Exe" or "WinExe" ? OutputKind.ConsoleApplication : OutputKind.DynamicallyLinkedLibrary, allowUnsafe: true,
        nullableContextOptions: NullableContextOptions.Disable));
// a signed project: friend access (InternalsVisibleTo with a public key) needs the same key
var keyFile = P("AssemblyOriginatorKeyFile");
if (!string.IsNullOrEmpty(keyFile) && !Path.IsPathRooted(keyFile))   // relative to the project folder
    keyFile = Path.GetFullPath(Path.Combine(items.GetProperty("Compile")[0].GetProperty("DefiningProjectDirectory").GetString(), keyFile));
if (P("SignAssembly") == "true" && File.Exists(keyFile))
    comp = comp.WithOptions(comp.Options.WithCryptoKeyFile(keyFile).WithPublicSign(true)
        .WithStrongNameProvider(new DesktopStrongNameProvider(ImmutableArray<string>.Empty, Path.GetTempPath())));
// --lib: compile an Exe project as a library (a test SDK adds a second entry point)
if (args.Contains("--lib")) comp = comp.WithOptions(comp.Options.WithOutputKind(OutputKind.DynamicallyLinkedLibrary));

int errs = 0;
var errFiles = new HashSet<string>();
foreach (var d in comp.GetDiagnostics())
    if (d.Severity == DiagnosticSeverity.Error)
    {
        errs++; if (errs <= 10) Console.Error.WriteLine(d.ToString());
        var ep = d.Location.SourceTree?.FilePath;
        if (ep != null) errFiles.Add(ep);
    }
Console.Error.WriteLine("cs errors: " + errs);

string Rel(string full) => full.StartsWith(root) ? full.Substring(root.Length) : null;
// a file outside the repo root that the build generated (obj, *.g.cs, generator output)
bool IsGen(string full) => !full.StartsWith(root) && (full.Contains("/obj/") || full.Contains("/generated/") || full.EndsWith(".g.cs") || full.EndsWith(".Designer.cs"));

// all named types declared in source, for the overridden test
var srcTypes = new List<INamedTypeSymbol>();
void Walk(INamespaceOrTypeSymbol n)
{
    foreach (var m in n.GetMembers())
    {
        if (m is INamespaceSymbol ns) Walk(ns);
        else if (m is INamedTypeSymbol t)
        {
            if (t.Locations.Any(l => l.IsInSource)) srcTypes.Add(t);
            Walk(t);
        }
    }
}
Walk(comp.Assembly.GlobalNamespace);

var subCache = new Dictionary<INamedTypeSymbol, List<INamedTypeSymbol>>(SymbolEqualityComparer.Default);
List<INamedTypeSymbol> Subs(INamedTypeSymbol t)
{
    t = t.OriginalDefinition;
    if (subCache.TryGetValue(t, out var l)) return l;
    l = new List<INamedTypeSymbol>();
    foreach (var s in srcTypes)
    {
        if (SymbolEqualityComparer.Default.Equals(s.OriginalDefinition, t)) continue;
        bool sub = false;
        for (var b = s.BaseType; b != null && !sub; b = b.BaseType)
            sub = SymbolEqualityComparer.Default.Equals(b.OriginalDefinition, t);
        if (!sub) sub = s.AllInterfaces.Any(i => SymbolEqualityComparer.Default.Equals(i.OriginalDefinition, t));
        if (sub) l.Add(s);
    }
    return subCache[t] = l;
}

bool Overridden(IMethodSymbol target, INamedTypeSymbol recv)
{
    var tg = target.OriginalDefinition;
    if (!(tg.IsVirtual || tg.IsAbstract || tg.IsOverride)) return false;
    foreach (var s in Subs(recv ?? tg.ContainingType))
    {
        if (tg.ContainingType.TypeKind == TypeKind.Interface)
        {
            var impl = s.FindImplementationForInterfaceMember(tg) as IMethodSymbol;
            if (impl != null && !SymbolEqualityComparer.Default.Equals(impl.OriginalDefinition, tg)
                && impl.Locations.Any(l => l.IsInSource)) return true;
        }
        foreach (var m in s.GetMembers(tg.Name).OfType<IMethodSymbol>())
        {
            if (!m.IsOverride || !m.Locations.Any(l => l.IsInSource)) continue;
            for (var o = m.OverriddenMethod; o != null; o = o.OverriddenMethod)
                if (SymbolEqualityComparer.Default.Equals(o.OriginalDefinition, tg)) return true;
        }
    }
    return false;
}

string Recv(SemanticModel m, ExpressionSyntax e)
{
    switch (e)
    {
        case ThisExpressionSyntax: return "this";
        case BaseExpressionSyntax: return "base";
        case IdentifierNameSyntax or GenericNameSyntax or MemberAccessExpressionSyntax or AliasQualifiedNameSyntax:
            var s = m.GetSymbolInfo(e).Symbol;
            if (s is INamedTypeSymbol or INamespaceSymbol or ITypeParameterSymbol) return "static";
            if (e is MemberAccessExpressionSyntax) return "chain";
            if (s is ILocalSymbol or IParameterSymbol or IRangeVariableSymbol) return "var";
            if (s is IFieldSymbol or IPropertySymbol or IEventSymbol) return "field";
            return "other";
        case InvocationExpressionSyntax or ElementAccessExpressionSyntax or ObjectCreationExpressionSyntax
            or ConditionalAccessExpressionSyntax: return "chain";
        default: return "other";
    }
}

string ShapeOf(SemanticModel m, ExpressionSyntax callee)
{
    switch (callee)
    {
        case IdentifierNameSyntax or GenericNameSyntax: return "bare";
        case MemberAccessExpressionSyntax ma: return Recv(m, ma.Expression);
        case MemberBindingExpressionSyntax mb:
            var ca = mb.Ancestors().OfType<ConditionalAccessExpressionSyntax>().FirstOrDefault(c => c.WhenNotNull.Span.Contains(mb.Span));
            return ca == null ? "other" : Recv(m, ca.Expression);
        default: return "other";
    }
}

ExpressionSyntax ReceiverOf(ExpressionSyntax callee) => callee switch
{
    MemberAccessExpressionSyntax ma => ma.Expression,
    MemberBindingExpressionSyntax mb => mb.Ancestors().OfType<ConditionalAccessExpressionSyntax>()
        .FirstOrDefault(c => c.WhenNotNull.Span.Contains(mb.Span))?.Expression,
    _ => null
};

int Line(SyntaxToken t) => t.GetLocation().GetLineSpan().StartLinePosition.Line + 1;

// declaration span of a symbol part: (file, start, end) or null when not under root
List<(string f, int s, int e)> Decl(ISymbol sym, bool header, SyntaxNode prefer = null)
{
    var l = new List<(string, int, int)>();
    foreach (var r in sym.DeclaringSyntaxReferences)
    {
        var n = r.GetSyntax();
        var f = Rel(r.SyntaxTree.FilePath);
        if (f == null) { l.Add((IsGen(r.SyntaxTree.FilePath) ? "<gen>" : null, 0, 0)); continue; }
        var ls = n.GetLocation().GetLineSpan();
        int s = ls.StartLinePosition.Line + 1, e = ls.EndLinePosition.Line + 1;
        if (header && n is BaseTypeDeclarationSyntax td)
        {
            // header end: the open brace, else the semicolon of a body-less type; a default token has kind None
            var tk = !td.OpenBraceToken.IsKind(SyntaxKind.None) && !td.OpenBraceToken.IsMissing ? td.OpenBraceToken
                : (n is TypeDeclarationSyntax tds && !tds.SemicolonToken.IsKind(SyntaxKind.None) && !tds.SemicolonToken.IsMissing ? tds.SemicolonToken
                : (n is EnumDeclarationSyntax eds && !eds.SemicolonToken.IsKind(SyntaxKind.None) && !eds.SemicolonToken.IsMissing ? eds.SemicolonToken : default));
            e = tk.IsKind(SyntaxKind.None) ? e : Line(tk);
        }
        e = Math.Max(e, s);   // an end line is never before the start line
        l.Add((f, s, e));
    }
    // For a constructor, the part that holds the constructor itself comes first (the primary row)
    if (header && prefer != null)
    {
        var pl = prefer.GetLocation().GetLineSpan();
        var pf = Rel(prefer.SyntaxTree.FilePath);
        int k = l.FindIndex(x => x.Item1 == pf && x.Item2 == pl.StartLinePosition.Line + 1);
        if (k > 0) { var t = l[k]; l.RemoveAt(k); l.Insert(0, t); }
    }
    return l;
}

var rows = new List<string>();
var inactive = new List<string>();
var stat = new Dictionary<string, int>();
void Bump(string k) => stat[k] = stat.GetValueOrDefault(k) + 1;
string J(object o) => JsonSerializer.Serialize(o);

foreach (var tree in trees)
{
    var src = Rel(tree.FilePath);
    if (src == null) continue;
    var model = comp.GetSemanticModel(tree);
    foreach (var tr in tree.GetRoot().DescendantTrivia(descendIntoTrivia: false).Where(x => x.IsKind(SyntaxKind.DisabledTextTrivia)))
    {
        var ls = tr.GetLocation().GetLineSpan();
        inactive.Add("{\"inactive\":true,\"src\":" + J(src) + ",\"start\":" + (ls.StartLinePosition.Line + 1) + ",\"end\":" + (ls.EndLinePosition.Line + 1) + "}");
    }
    foreach (var node in tree.GetRoot().DescendantNodes())
    {
        ExpressionSyntax callee = null;
        bool isNew = false;
        if (node is InvocationExpressionSyntax inv) callee = inv.Expression;
        else if (node is BaseObjectCreationExpressionSyntax) isNew = true;
        else continue;

        var info = model.GetSymbolInfo(node);
        var sym = info.Symbol as IMethodSymbol;
        bool dyn = false;
        if (!isNew)
        {
            var rcv = ReceiverOf(callee);
            if (rcv != null && model.GetTypeInfo(rcv).Type?.TypeKind == TypeKind.Dynamic) dyn = true;
            if (node is InvocationExpressionSyntax iv && iv.ArgumentList.Arguments.Any(a => model.GetTypeInfo(a.Expression).Type?.TypeKind == TypeKind.Dynamic)) dyn = true;
        }
        INamedTypeSymbol newType = null;
        if (isNew) newType = model.GetTypeInfo(node).Type as INamedTypeSymbol;
        if (sym == null)
        {
            // Nameof is an invocation with no symbol
            if (callee is IdentifierNameSyntax { Identifier.Text: "nameof" }) { Bump("nameof"); continue; }
            // An error row. Never take CandidateSymbols[0]. LateBound and OverloadResolutionFailure make any edge wrong; the rest are unverified.
            var cr = info.CandidateReason;
            if (info.CandidateSymbols.Length > 0 || cr != CandidateReason.None)
            {
                string flagName = cr is CandidateReason.LateBound or CandidateReason.OverloadResolutionFailure ? "badcall" : "unv";
                int cl = isNew ? ((BaseObjectCreationExpressionSyntax)node).NewKeyword.GetLocation().GetLineSpan().StartLinePosition.Line + 1 : (callee is MemberAccessExpressionSyntax cm ? Line(cm.Name.Identifier) : callee.GetLocation().GetLineSpan().StartLinePosition.Line + 1);
                rows.Add("{\"src\":" + J(src) + ",\"line\":" + cl + ",\"shape\":\"other\",\"name\":" + J(isNew ? (newType?.Name ?? "") : (info.CandidateSymbols.FirstOrDefault()?.Name ?? "")) + ",\"dst\":\"\",\"dst_start\":0,\"dst_end\":0,\"" + flagName + "\":true,\"reason\":\"" + cr + "\"}");
                Bump("cand:" + cr);
                continue;
            }
            if (newType == null) { Bump("nosym"); continue; }
        }

        int line = isNew ? ((BaseObjectCreationExpressionSyntax)node).NewKeyword.GetLocation().GetLineSpan().StartLinePosition.Line + 1
            : (callee is MemberAccessExpressionSyntax mx ? Line(mx.Name.Identifier)
               : callee is MemberBindingExpressionSyntax mbx ? Line(mbx.Name.Identifier)
               : callee.GetLocation().GetLineSpan().StartLinePosition.Line + 1);
        string shape = isNew ? "new" : ShapeOf(model, callee);
        string name = sym != null ? (sym.MethodKind == MethodKind.Constructor ? sym.ContainingType.Name : sym.Name) : newType.Name;
        var fl = new Dictionary<string, object>();
        if (sym != null)
        {
            var o = (sym.ReducedFrom ?? sym).OriginalDefinition;   // A reduced extension maps to the extension declaration
            if (o.IsStatic) fl["static"] = true;
            if (o.IsVirtual) fl["virtual"] = true;
            if (o.IsAbstract || o.ContainingType?.TypeKind == TypeKind.Interface) fl["abs"] = true;
            if (o.IsOverride) fl["override"] = true;
            if (o.IsSealed) fl["sealed"] = true;
            if (o.ContainingType?.TypeKind == TypeKind.Interface) fl["iface"] = true;
            if (o.IsExtensionMethod || sym.ReducedFrom != null) fl["ext"] = true;
            if (o.MethodKind == MethodKind.LocalFunction) fl["local"] = true;
            if (sym.MethodKind == MethodKind.DelegateInvoke) fl["deleg"] = true;
            if (dyn) fl["dyn"] = true;
            if (!isNew)
            {
                try
                {
                    if (node is InvocationExpressionSyntax ic && model.GetInterceptorMethod(ic) != null) fl["intercepted"] = true;
                }
                catch (Exception) { }
            }
            if (!isNew && shape != "base" && !fl.ContainsKey("ext"))   // A base call is exact
            {
                INamedTypeSymbol rt = null;
                var rcv = ReceiverOf(callee);
                if (rcv != null) rt = model.GetTypeInfo(rcv).Type as INamedTypeSymbol;
                else rt = model.GetEnclosingSymbol(node.SpanStart)?.ContainingType;
                if (Overridden(sym, rt)) fl["ovr"] = true;
            }
        }
        else if (dyn) fl["dyn"] = true;

        var target = sym != null && sym.MethodKind != MethodKind.Constructor ? (ISymbol)(sym.ReducedFrom ?? sym).OriginalDefinition
            : (ISymbol)(sym?.ContainingType ?? newType).OriginalDefinition;
        bool ctor = target is INamedTypeSymbol;
        var decls = Decl(target, ctor, ctor ? sym?.DeclaringSyntaxReferences.FirstOrDefault()?.GetSyntax()?.Parent : null);
        string head = "{\"src\":" + J(src) + ",\"line\":" + line + ",\"shape\":" + J(shape) + ",\"name\":" + J(name);
        string tail = string.Concat(fl.Select(kv => ",\"" + kv.Key + "\":true"));
        Bump("shape:" + shape);
        if (decls.Count == 0)
        {
            rows.Add(head + ",\"dst\":\"\",\"dst_start\":0,\"dst_end\":0,\"" + (sym != null && sym.MethodKind == MethodKind.DelegateInvoke || target.IsImplicitlyDeclared ? "impl" : "oor") + "\":true" + tail + "}");
            continue;
        }
        bool first = true;
        foreach (var (f, s, e) in decls)
        {
            if (f == "<gen>") { if (first) { rows.Add(head + ",\"dst\":\"\",\"dst_start\":0,\"dst_end\":0,\"gen\":true" + tail + "}"); first = false; } continue; }
            if (f == null) { if (first) { rows.Add(head + ",\"dst\":\"\",\"dst_start\":0,\"dst_end\":0,\"oor\":true" + tail + "}"); first = false; } continue; }
            rows.Add(head + ",\"dst\":" + J(f) + ",\"dst_start\":" + s + ",\"dst_end\":" + e + tail + (first ? "" : ",\"alt\":true") + "}");
            first = false;
        }
    }
}
var errRows = errFiles.Select(f => Rel(f)).Where(f => f != null).Select(f => "{\"errfile\":true,\"src\":" + J(f) + "}").ToList();
int errfileRows = rows.Count(r => errFiles.Any(f => Rel(f) != null && r.StartsWith("{\"src\":" + J(Rel(f)) + ",")));
Console.Error.WriteLine($"rows in error files: {errfileRows}; ext rows: {rows.Count(r => r.Contains("\"ext\":true"))}; base rows: {rows.Count(r => r.Contains("\"shape\":\"base\""))}; gen rows: {rows.Count(r => r.Contains("\"gen\":true"))}; deleg rows: {rows.Count(r => r.Contains("\"deleg\":true"))}; local rows: {rows.Count(r => r.Contains("\"local\":true"))}; dyn rows: {rows.Count(r => r.Contains("\"dyn\":true"))}; intercepted rows: {rows.Count(r => r.Contains("\"intercepted\":true"))}");
File.WriteAllLines(outPath ?? "/dev/stdout", rows.Concat(inactive).Concat(errRows));
Console.Error.WriteLine("rows: " + rows.Count + "; " + string.Join(", ", stat.OrderBy(k => k.Key).Select(k => k.Key + "=" + k.Value)));
return errs > 0 ? 1 : 0;
