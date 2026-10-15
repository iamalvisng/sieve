// Oracle for Java call links. Usage (run in the repo root, JDK 21):
//   java JvCalls.java <srcroot>... [-cp <classpath>] [--release N] [-t <testroot>]... [-o rows.jsonl]
// Compiles every .java file under the source roots in one javac task (module-info.java
// is skipped) and writes one JSON row per method call or `new` expression whose
// target has a source declaration in the given roots:
//   {src, line, shape, dst, name, dst_start, dst_end, abs?}
// Paths are relative to the working directory. Lines are 1-based. dst_start and
// dst_end are the first and last line of the declaration. For a class, dst_end is
// the line before its first member (the header only), so a method in the class
// does not match a constructor row. A constructor maps to its class. A target with
// no source (JDK, jar, implicit values(), default constructor of a class that has
// none) is dropped and counted. abs is true when the target is an interface or
// abstract method. Other flags (only when true): iface (owner is an interface), static,
// final, ovr (an in-repo subtype of the receiver's static type declares a method that
// overrides the target: the runtime target may differ), anon (new with a class body).
// A call whose target has no source tree gets a row with oor:true (out of repo: JDK or jar)
// or impl:true (in repo, no tree: values(), a record accessor); dst is "" for oor.
// -t roots are test roots. If javac reports errors, the run repeats on the other roots
// only and prints "test roots dropped" (the caller then treats test files as unverified).
// -proc:none stops annotation processors. module-info.java is skipped. Shapes: bare, this, super, static, var, field, new, chain.
// Every javac diagnostic goes to stderr, then "javac errors: N". Exit 1 if N > 0.
import com.sun.source.tree.*;
import com.sun.source.util.*;
import javax.lang.model.element.*;
import javax.lang.model.util.*;
import javax.tools.*;
import java.io.*;
import java.nio.file.*;
import java.util.*;
import java.util.stream.*;

public class JvCalls {
    static Trees trees;
    static Path cwd = Paths.get("").toAbsolutePath();
    static Set<String> seen = new HashSet<>();
    static List<String> rows = new ArrayList<>();
    static int noSrc = 0, impl = 0, oor = 0;
    static Types types;
    static Elements elements;
    static List<TypeElement> allTypes = new ArrayList<>();
    static Map<TypeElement, List<TypeElement>> subsCache = new HashMap<>();

    public static void main(String[] a) throws Exception {
        List<Path> roots = new ArrayList<>(), troots = new ArrayList<>();
        String cp = "", out = null, release = null;
        for (int i = 0; i < a.length; i++) {
            if (a[i].equals("-cp")) cp = a[++i];
            else if (a[i].equals("-o")) out = a[++i];
            else if (a[i].equals("--release")) release = a[++i];
            else if (a[i].equals("-t")) troots.add(Paths.get(a[++i]));
            else roots.add(Paths.get(a[i]));
        }
        int errs = run(concat(roots, troots), cp, release);
        if (errs > 0 && !troots.isEmpty()) {
            System.err.println("javac errors: " + errs + "; test roots dropped, retry on the other roots");
            rows.clear(); seen.clear(); noSrc = 0; impl = 0;
            errs = run(roots, cp, release);
        }
        PrintStream ps = out == null ? System.out : new PrintStream(new FileOutputStream(out));
        for (String r : rows) ps.println(r);
        ps.flush();
        System.err.println("rows: " + rows.size() + "; dropped (no element): " + noSrc + "; impl rows: " + impl + "; oor rows: " + oor);
        System.err.println("javac errors: " + errs);
        System.exit(errs > 0 ? 1 : 0);
    }

    static List<Path> concat(List<Path> a, List<Path> b) { List<Path> l = new ArrayList<>(a); l.addAll(b); return l; }

    static int run(List<Path> roots, String cp, String release) throws Exception {
        List<File> files = new ArrayList<>();
        for (Path r : roots)
            try (Stream<Path> s = Files.walk(r)) {
                s.filter(p -> p.toString().endsWith(".java") && !p.getFileName().toString().equals("module-info.java"))
                        .sorted().forEach(p -> files.add(p.toFile()));
            }
        JavaCompiler jc = ToolProvider.getSystemJavaCompiler();
        int[] errs = {0};
        DiagnosticListener<JavaFileObject> dl = d -> {
            if (d.getKind() == Diagnostic.Kind.ERROR) errs[0]++;
            System.err.println(d);
        };
        StandardJavaFileManager fm = jc.getStandardFileManager(dl, null, null);
        List<String> opts = new ArrayList<>(List.of("-proc:none", "-Xlint:none", "-implicit:none", "-g:none"));
        if (!cp.isEmpty()) { opts.add("-cp"); opts.add(cp); }
        if (release != null) { opts.add("--release"); opts.add(release); }
        JavacTask task = (JavacTask) jc.getTask(null, fm, dl, opts, null, fm.getJavaFileObjectsFromFiles(files));
        trees = Trees.instance(task);
        types = task.getTypes();
        elements = task.getElements();
        Iterable<? extends CompilationUnitTree> units = task.parse();
        task.analyze();
        allTypes.clear(); subsCache.clear();
        for (CompilationUnitTree cu : units) new Collect(cu).scan(new TreePath(cu), null);
        for (CompilationUnitTree cu : units) new Scan(cu).scan(new TreePath(cu), null);
        return errs[0];
    }

    static class Collect extends TreePathScanner<Void, Void> {
        Collect(CompilationUnitTree cu) {}
        @Override public Void visitClass(ClassTree n, Void v) {
            Element e = trees.getElement(getCurrentPath());
            if (e instanceof TypeElement t) allTypes.add(t);
            return super.visitClass(n, v);
        }
    }

    static List<TypeElement> subs(TypeElement r) {
        return subsCache.computeIfAbsent(r, k -> {
            List<TypeElement> l = new ArrayList<>();
            javax.lang.model.type.TypeMirror er = types.erasure(k.asType());
            for (TypeElement t : allTypes)
                if (!t.equals(k) && types.isSubtype(types.erasure(t.asType()), er)) l.add(t);
            return l;
        });
    }

    static class Scan extends TreePathScanner<Void, Void> {
        final CompilationUnitTree cu;
        Scan(CompilationUnitTree cu) { this.cu = cu; }

        long pos(Tree t) { return trees.getSourcePositions().getStartPosition(cu, t); }
        long end(Tree t) { return trees.getSourcePositions().getEndPosition(cu, t); }
        long line(long p) { return ln(cu, p); }

        @Override public Void visitMethodInvocation(MethodInvocationTree n, Void v) {
            ExpressionTree sel = n.getMethodSelect();
            Element e = trees.getElement(new TreePath(getCurrentPath(), sel));
            String name, shape;
            long at;
            TypeElement recv = null;
            if (sel instanceof IdentifierTree id) {
                name = id.getName().toString();
                shape = name.equals("this") ? "this" : name.equals("super") ? "super" : "bare";
                at = pos(sel);
                if (shape.equals("bare")) recv = enclosingClass();
            } else if (sel instanceof MemberSelectTree ms) {
                name = ms.getIdentifier().toString();
                shape = recvShape(ms.getExpression());
                at = end(sel);
                if (shape.equals("this")) recv = enclosingClass();
                else if (!shape.equals("super") && !shape.equals("static")) recv = typeOf(ms.getExpression());
            } else { name = "?"; shape = "chain"; at = pos(sel); }
            if (e == null) noSrc++;
            else emit(e, shape, at, name, recv, false);
            return super.visitMethodInvocation(n, v);
        }

        TypeElement enclosingClass() {
            for (TreePath p = getCurrentPath(); p != null; p = p.getParentPath())
                if (p.getLeaf() instanceof ClassTree && trees.getElement(p) instanceof TypeElement t) return t;
            return null;
        }

        TypeElement typeOf(ExpressionTree x) {
            var tm = trees.getTypeMirror(new TreePath(getCurrentPath(), x));
            if (tm == null) return null;
            tm = types.erasure(tm);  // a type variable becomes its bound
            return types.asElement(tm) instanceof TypeElement t ? t : null;
        }

        String recvShape(ExpressionTree x) {
            while (x instanceof ParenthesizedTree p) x = p.getExpression();
            if (x instanceof IdentifierTree id) {
                String s = id.getName().toString();
                if (s.equals("this")) return "this";
                if (s.equals("super")) return "super";
            }
            if (x instanceof IdentifierTree || x instanceof MemberSelectTree) {
                Element r = trees.getElement(new TreePath(getCurrentPath(), x));
                if (r == null) return "chain";
                ElementKind k = r.getKind();
                if (k.isClass() || k.isInterface()) return "static";
                if (k == ElementKind.FIELD || k == ElementKind.ENUM_CONSTANT) return "field";
                if (k == ElementKind.PACKAGE) return "chain";
                return "var";
            }
            return "chain";
        }

        @Override public Void visitNewClass(NewClassTree n, Void v) {
            boolean body = n.getClassBody() != null;
            // With a class body the target is the anonymous class, not the named type
            Element e = trees.getElement(body ? getCurrentPath() : new TreePath(getCurrentPath(), n.getIdentifier()));
            if (e != null && e.getKind() == ElementKind.CONSTRUCTOR) e = e.getEnclosingElement();
            if (e instanceof TypeElement) emit(e, "new", pos(n), e.getSimpleName().toString(), null, body);
            return super.visitNewClass(n, v);
        }

        boolean overridden(Element target, TypeElement recv) {
            if (recv == null || target.getKind() != ElementKind.METHOD) return false;
            Set<Modifier> m = target.getModifiers();
            if (m.contains(Modifier.STATIC) || m.contains(Modifier.PRIVATE) || m.contains(Modifier.FINAL)) return false;
            for (TypeElement sub : subs(recv))
                for (Element c : sub.getEnclosedElements())
                    if (c instanceof ExecutableElement ce && c.getKind() == ElementKind.METHOD
                            && c.getSimpleName().equals(target.getSimpleName())
                            && elements.overrides(ce, (ExecutableElement) target, sub)) return true;
            return false;
        }

        void emit(Element e, String shape, long at, String name, TypeElement recv, boolean anon) {
            Element d = e.getKind() == ElementKind.CONSTRUCTOR ? e.getEnclosingElement() : e;
            String src = rel(cu.getSourceFile());
            TreePath dp = trees.getPath(d);
            if (dp == null) {
                // No tree. impl = in repo (values(), record accessor); oor = JDK or jar.
                boolean inRepo = trees.getPath(d.getEnclosingElement()) != null
                        || (d.getEnclosingElement() != null && d.getEnclosingElement().getEnclosingElement() != null
                        && trees.getPath(d.getEnclosingElement().getEnclosingElement()) != null);
                String k = inRepo ? "impl" : "oor";
                if (inRepo) impl++; else oor++;
                String key = src + ":" + line(at) + ":" + shape + ":" + k + ":" + d.getSimpleName();
                if (seen.add(key))
                    rows.add("{\"src\":" + q(src) + ",\"line\":" + line(at) + ",\"shape\":\"" + shape + "\",\"dst\":\"\",\"name\":"
                            + q(d.getSimpleName().toString()) + ",\"dst_start\":0,\"dst_end\":0,\"" + k + "\":true}");
                return;
            }
            CompilationUnitTree dcu = dp.getCompilationUnit();
            Tree dt = dp.getLeaf();
            long s = trees.getSourcePositions().getStartPosition(dcu, dt);
            long en = trees.getSourcePositions().getEndPosition(dcu, dt);
            if (s < 0) { noSrc++; return; }
            long sl = ln(dcu, s), el = ln(dcu, en);
            if (dt instanceof ClassTree ct) {
                long first = -1;
                for (Tree m : ct.getMembers()) {
                    long p = trees.getSourcePositions().getStartPosition(dcu, m);
                    if (p > s && (first < 0 || p < first)) first = p;  // the default constructor has no own position
                }
                el = first < 0 ? sl : Math.max(sl, ln(dcu, first) - 1);
            }
            Set<Modifier> mods = d.getModifiers();
            boolean iface = d.getEnclosingElement() != null && d.getEnclosingElement().getKind().isInterface()
                    && d.getKind() == ElementKind.METHOD;
            boolean abs = mods.contains(Modifier.ABSTRACT)
                    || (iface && !mods.contains(Modifier.DEFAULT) && !mods.contains(Modifier.STATIC) && !mods.contains(Modifier.PRIVATE));
            boolean ovr = !shape.equals("super") && overridden(d, recv);
            String dst = rel(dcu.getSourceFile());
            String dn = d.getSimpleName().toString();
            String key = src + ":" + line(at) + ":" + shape + ":" + dst + ":" + sl + ":" + dn;
            if (!seen.add(key)) return;
            rows.add("{\"src\":" + q(src) + ",\"line\":" + line(at) + ",\"shape\":\"" + shape + "\",\"dst\":" + q(dst)
                    + ",\"name\":" + q(dn) + ",\"dst_start\":" + sl + ",\"dst_end\":" + el
                    + (abs ? ",\"abs\":true" : "") + (iface ? ",\"iface\":true" : "")
                    + (mods.contains(Modifier.STATIC) ? ",\"static\":true" : "") + (mods.contains(Modifier.FINAL) ? ",\"final\":true" : "")
                    + (ovr ? ",\"ovr\":true" : "") + (anon ? ",\"anon\":true" : "") + "}");
        }
    }

    static Map<JavaFileObject, int[]> starts = new HashMap<>();

    // Line of a char offset. The LineMap of a unit can be null after analyze(), so count newlines.
    static long ln(CompilationUnitTree cu, long p) {
        int[] st = starts.computeIfAbsent(cu.getSourceFile(), f -> {
            try {
                CharSequence t = f.getCharContent(true);
                List<Integer> l = new ArrayList<>(List.of(0));
                for (int i = 0; i < t.length(); i++) if (t.charAt(i) == '\n') l.add(i + 1);
                return l.stream().mapToInt(Integer::intValue).toArray();
            } catch (IOException e) { throw new UncheckedIOException(e); }
        });
        int i = Arrays.binarySearch(st, (int) p);
        return i >= 0 ? i + 1 : -i - 1;
    }

    static String rel(JavaFileObject f) {
        Path p = Paths.get(f.toUri()).toAbsolutePath().normalize();
        return cwd.relativize(p).toString();
    }

    static String q(String s) { return "\"" + s.replace("\\", "\\\\").replace("\"", "\\\"") + "\""; }
}
