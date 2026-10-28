// Oracle for cross-file calls. Usage: node ts-calls.mjs <repo>
// The TypeScript compiler resolves each call that reaches a symbol in
// another repo file through an import. The script prints one JSON row per
// call: {src, line, shape, barrel, dst, name, dst_line}. A function passed as a call
// argument (items.map(helper), fn(ns.helper)) is one more row with arg true. Paths are relative
// to <repo>. The walk skips node_modules, dist and dot-directories.
// shape is named, alias, default, namespace or nsexport. barrel is true when the
// import chain passes a re-export. spec is the module specifier text.
// An nsexport row has name null when the target has no name.
// Rows sort by src, line, dst, name. The script sets its own compiler
// options and reads no tsconfig.json.
import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";

const ts = createRequire(import.meta.url)("typescript");
const root = path.resolve(process.argv[2] ?? ".");
// Same list as SKIP_DIRS in crates/sieve-core/src/walk.rs, plus dot-directories.
// A Sieve includeDirs config is not supported.
const SKIP = new Set(["node_modules", "dist", "build", "_build", "out", "target",
  "vendor", "coverage", "__pycache__", "venv"]);
const EXT = /\.(ts|tsx|js|jsx|mjs|cjs|mts|cts)$/;

function walk(dir, out) {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) {
      if (!SKIP.has(e.name) && !e.name.startsWith(".")) walk(p, out);
    } else if (EXT.test(e.name) && !e.name.endsWith(".d.ts")) out.push(p);
  }
  return out;
}

const program = ts.createProgram(walk(root, []), {
  allowJs: true,
  noEmit: true,
  skipLibCheck: true,
  jsx: ts.JsxEmit.Preserve,
  module: ts.ModuleKind.ESNext,
  moduleResolution: ts.ModuleResolutionKind.Bundler,
});
const checker = program.getTypeChecker();
const rel = (f) => path.relative(root, f).split(path.sep).join("/");
const inRepo = (f) => !rel(f).startsWith("..") && !rel(f).includes("node_modules/");
const fileOf = (sym) => sym?.declarations?.[0]?.getSourceFile().fileName;

// The shape of an import binding, from its first declaration.
function shapeOf(sym) {
  const d = sym?.declarations?.[0];
  if (!d) return null;
  if (ts.isImportClause(d)) return "default";
  if (ts.isNamespaceImport(d)) return "namespace";
  if (!ts.isImportSpecifier(d)) return null;
  if (!d.propertyName) return "named";
  return d.propertyName.text === "default" ? "default" : "alias";
}

// The import declaration of a binding symbol.
function importOf(sym) {
  for (let n = sym.declarations[0]; n; n = n.parent) if (ts.isImportDeclaration(n)) return n;
}

// A module file symbol. A namespace block also has ValueModule, but no SourceFile declaration.
const isModuleFile = (sym) =>
  !!(sym.flags & ts.SymbolFlags.ValueModule) && !!sym.declarations?.some((d) => ts.isSourceFile(d));

// Follow the alias chain. files holds the file of every link.
function resolve(sym) {
  const files = new Set();
  while (sym.flags & ts.SymbolFlags.Alias) {
    files.add(fileOf(sym));
    const next = checker.getImmediateAliasedSymbol(sym);
    if (!next) break;
    sym = next;
  }
  return { sym, files };
}

const rows = [];
const isFunctionDecl = (d) =>
  !!d && (ts.isFunctionDeclaration(d) || ts.isMethodDeclaration(d) ||
    (ts.isVariableDeclaration(d) && !!d.initializer && (ts.isArrowFunction(d.initializer) || ts.isFunctionExpression(d.initializer))));

// One row for the expression c: a callee, or (arg true) a function passed as a call argument.
function addRow(c, node, sf, arg) {
  {
    let shape, target, bind;
    if (ts.isIdentifier(c)) {
      bind = checker.getSymbolAtLocation(c);
      shape = shapeOf(bind);
      target = bind;
    } else if (ts.isPropertyAccessExpression(c) && ts.isIdentifier(c.expression)) {
      bind = checker.getSymbolAtLocation(c.expression);
      const bs = shapeOf(bind);
      shape = bs === "namespace" ? "namespace" : null;
      // A named or alias binding that aliases to a value module is a namespace export.
      if (!shape && (bs === "named" || bs === "alias") && isModuleFile(resolve(bind).sym)) shape = "nsexport";
      if (shape) target = checker.getSymbolAtLocation(c.name);
    }
    if (shape && target) {
      const { sym, files } = resolve(target);
      const d = sym.declarations?.[0];
      const dsf = d?.getSourceFile();
      const name = d && !ts.isSourceFile(d) ? (d.name?.text ?? null) : null;
      const imp = importOf(bind);
      // A passed value counts only when the target declares a function.
      if (arg && !isFunctionDecl(d)) return;
      // An nsexport row keeps a null name for an anonymous target. Other shapes skip it.
      if ((name || (shape === "nsexport" && d && !ts.isSourceFile(d))) && imp && inRepo(dsf.fileName) && dsf.fileName !== sf.fileName) {
        // The module file of the specifier joins the chain: it finds export * barrels.
        files.add(fileOf(checker.getSymbolAtLocation(imp.moduleSpecifier)));
        files.delete(sf.fileName);
        files.delete(dsf.fileName);
        files.delete(undefined);
        rows.push({
          src: rel(sf.fileName),
          line: sf.getLineAndCharacterOfPosition(node.getStart()).line + 1,
          ...(arg ? { arg: true } : {}),
          shape,
          spec: imp.moduleSpecifier.text,
          barrel: files.size > 0,
          dst: rel(dsf.fileName),
          name,
          dst_line: dsf.getLineAndCharacterOfPosition(d.getStart()).line + 1,
        });
      }
    }
  }
}

function visit(node, sf) {
  if (ts.isCallExpression(node)) {
    addRow(node.expression, node, sf, false);
    // A function passed as a value: an identifier or a ns.name path in an argument position.
    for (const a of node.arguments) {
      if (ts.isIdentifier(a) || (ts.isPropertyAccessExpression(a) && ts.isIdentifier(a.expression))) addRow(a, a, sf, true);
    }
  }
  ts.forEachChild(node, (n) => visit(n, sf));
}

for (const sf of program.getSourceFiles()) {
  if (!sf.isDeclarationFile && inRepo(sf.fileName)) visit(sf, sf);
}
const cmp = (a, b) => (a < b ? -1 : a > b ? 1 : 0);
rows.sort((a, b) => cmp(a.src, b.src) || a.line - b.line || cmp(a.dst, b.dst) || cmp(a.name, b.name));
for (const r of rows) console.log(JSON.stringify(r));
