// Oracle plugin for Kotlin call targets. It is a K2 (FIR) compiler plugin for kotlinc 2.4.21.
// Build and run it with scripts/oracle/kt-calls.sh. The plugin writes one JSON row per call
// to the file named by the KT_OUT variable. KT_ROOT is the repo root: paths in rows are
// relative to it. Lines are 1-based.
//
// Call row: {src, line, name, shape, dst, dst_start, dst_end, dkey, flags...}
//   shape: bare, this, super, new, op, chain, deleg (this(...) or super(...) call)
//   dst is "" when the target has no repo source. dst_start and dst_end are the first and
//   last line of the declaration. A class target ends on the line of its first "{", so a
//   member of the class does not match a class row. A constructor maps to its class.
//   Flags (only when true): ctor, ext (extension), fnval (invoke on a function value),
//   lambda (call of a lambda), local (local function), top (top-level function), abs
//   (abstract member), iface (member of an interface), ovd (the target is an override),
//   virt (open, abstract or override member), oor (target outside the repo), impl (in
//   repo, no tree: data class copy, enum values), err (compiler error, no target),
//   java (target in Java source), op (operator, infix or indexing call), expect (the target
//   is an expect declaration: the runtime target is its actual), annot (annotation class).
// Decl row: {kind:"decl", dkind, name, owner, file, start, end}
//   dkind: class, interface, object, companion, enum, function, constructor, property.
//   owner: the outer named classes and functions, innermost last, empty at top level. A
//   companion without a name has the name "Companion". start is the first token line (as
//   above). end is the last line of the whole declaration. Compiler-made declarations
//   (data class copy, enum values, default constructors) and local variables have no row.
// Override row: {kind:"ovrfact", sub, base}. Both are "file:line" of the declaration start.
// The compare adds the ovr flag to a call row when an in-repo function overrides the target,
// directly or through a chain.
// A line offset starts at the first token of the declaration: KDoc and comments in front
// are skipped, annotations and modifiers stay in.
@file:OptIn(ExperimentalCompilerApi::class, SymbolInternals::class)

package sieve.ktcalls

import org.jetbrains.kotlin.KtFakeSourceElementKind
import org.jetbrains.kotlin.KtSourceElement
import org.jetbrains.kotlin.KtSourceFile
import org.jetbrains.kotlin.compiler.plugin.CompilerPluginRegistrar
import org.jetbrains.kotlin.compiler.plugin.ExperimentalCompilerApi
import org.jetbrains.kotlin.config.CompilerConfiguration
import org.jetbrains.kotlin.diagnostics.DiagnosticReporter
import org.jetbrains.kotlin.fir.FirSession
import org.jetbrains.kotlin.fir.SessionHolder
import org.jetbrains.kotlin.fir.analysis.checkers.MppCheckerKind
import org.jetbrains.kotlin.fir.analysis.checkers.context.CheckerContext
import org.jetbrains.kotlin.fir.analysis.checkers.declaration.DeclarationCheckers
import org.jetbrains.kotlin.fir.analysis.checkers.declaration.FirDeclarationChecker
import org.jetbrains.kotlin.fir.analysis.checkers.directOverriddenFunctionsSafe
import org.jetbrains.kotlin.fir.analysis.checkers.expression.ExpressionCheckers
import org.jetbrains.kotlin.fir.analysis.checkers.expression.FirExpressionChecker
import org.jetbrains.kotlin.fir.analysis.extensions.FirAdditionalCheckersExtension
import org.jetbrains.kotlin.fir.declarations.FirDeclarationOrigin
import org.jetbrains.kotlin.fir.declarations.FirConstructor
import org.jetbrains.kotlin.fir.declarations.FirNamedFunction
import org.jetbrains.kotlin.fir.declarations.FirProperty
import org.jetbrains.kotlin.fir.declarations.FirRegularClass
import org.jetbrains.kotlin.fir.declarations.utils.isCompanion
import org.jetbrains.kotlin.fir.declarations.utils.isAbstract
import org.jetbrains.kotlin.fir.declarations.utils.isExpect
import org.jetbrains.kotlin.fir.declarations.utils.isOpen
import org.jetbrains.kotlin.fir.declarations.utils.isOverride
import org.jetbrains.kotlin.fir.expressions.FirCall
import org.jetbrains.kotlin.fir.expressions.FirDelegatedConstructorCall
import org.jetbrains.kotlin.fir.expressions.FirFunctionCall
import org.jetbrains.kotlin.fir.expressions.FirSuperReceiverExpression
import org.jetbrains.kotlin.fir.expressions.FirThisReceiverExpression
import org.jetbrains.kotlin.fir.extensions.FirExtensionRegistrar
import org.jetbrains.kotlin.fir.extensions.FirExtensionRegistrarAdapter
import org.jetbrains.kotlin.fir.references.isError
import org.jetbrains.kotlin.fir.references.toResolvedCallableSymbol
import org.jetbrains.kotlin.fir.resolve.providers.firProvider
import org.jetbrains.kotlin.fir.resolve.toClassSymbol
import org.jetbrains.kotlin.fir.resolve.providers.symbolProvider
import org.jetbrains.kotlin.fir.symbols.SymbolInternals
import org.jetbrains.kotlin.fir.symbols.impl.FirCallableSymbol
import org.jetbrains.kotlin.fir.symbols.impl.FirClassLikeSymbol
import org.jetbrains.kotlin.fir.symbols.impl.FirConstructorSymbol
import org.jetbrains.kotlin.fir.symbols.impl.FirNamedFunctionSymbol
import org.jetbrains.kotlin.fir.unwrapFakeOverrides
import org.jetbrains.kotlin.descriptors.ClassKind
import org.jetbrains.kotlin.fir.symbols.impl.FirRegularClassSymbol
import java.io.File
import java.io.PrintWriter

private val root: String = (System.getenv("KT_ROOT") ?: ".").let { File(it).canonicalPath } + "/"
private val outFile: String = System.getenv("KT_OUT") ?: "kt-rows.jsonl"
private val writer = PrintWriter(java.io.FileWriter(outFile, true))
private val seen = HashSet<String>()
private val texts = HashMap<String, String>()
private val lineStarts = HashMap<String, IntArray>()

private fun emit(row: String) = synchronized(seen) {
    if (seen.add(row)) {
        writer.println(row)
        writer.flush()
    }
}

private fun q(s: String) = "\"" + s.replace("\\", "\\\\").replace("\"", "\\\"").replace("\n", " ") + "\""

private fun rel(abs: String): String {
    val c = File(abs).canonicalPath
    return if (c.startsWith(root)) c.substring(root.length) else c
}

private fun textOf(path: String): String = synchronized(texts) {
    texts.getOrPut(path) { try { File(if (path.startsWith("/")) path else root + path).readText() } catch (e: Exception) { "" } }
}

private fun lineOf(path: String, off: Int): Int {
    val starts = synchronized(lineStarts) {
        lineStarts.getOrPut(path) {
            val t = textOf(path)
            val l = ArrayList<Int>()
            l.add(0)
            for (i in t.indices) if (t[i] == '\n') l.add(i + 1)
            l.toIntArray()
        }
    }
    var lo = 0
    var hi = starts.size - 1
    while (lo < hi) {
        val mid = (lo + hi + 1) / 2
        if (starts[mid] <= off) lo = mid else hi = mid - 1
    }
    return lo + 1
}

// Move past blanks, line comments and block comments (KDoc included).
private fun skipTrivia(t: String, from: Int): Int {
    var i = from
    while (i < t.length) {
        if (t[i].isWhitespace()) i++
        else if (t.startsWith("//", i)) { while (i < t.length && t[i] != '\n') i++ }
        else if (t.startsWith("/*", i)) {
            val e = t.indexOf("*/", i + 2)
            i = if (e < 0) t.length else e + 2
        } else break
    }
    return i
}

private class Target(val file: String, val start: Int, val end: Int)

private fun declTarget(file: KtSourceFile?, src: KtSourceElement?, isClass: Boolean): Target? {
    if (file == null || src == null) return null
    val path = rel(file.path ?: return null)
    val t = textOf(path)
    val s = skipTrivia(t, src.startOffset)
    val sl = lineOf(path, s)
    var el = lineOf(path, maxOf(s, src.endOffset - 1))
    // The span is the header: it ends on the line of the first "{" (or "=" for a function) at
    // paren depth 0. A member or a nested function in the body then never matches. If there is
    // no such char, the span ends on the last line of the declaration.
    var depth = 0
    var i = s
    while (i < src.endOffset && i < t.length) {
        val c = t[i]
        if (c == '"') {
            i++
            while (i < t.length && t[i] != '"' && t[i] != '\n') { if (t[i] == '\\') i++; i++ }
        } else if (t.startsWith("//", i) || t.startsWith("/*", i)) {
            i = skipTrivia(t, i)
            continue
        } else if (c == '(') depth++
        else if (c == ')') depth--
        else if (depth == 0 && (c == '{' || (!isClass && c == '=' && t.getOrNull(i + 1) != '='))) { el = lineOf(path, i); break }
        i++
    }
    return Target(path, sl, el)
}

// A declaration's own module session finds its file. A common-fragment declaration is not in
// the provider of the fragment that calls it.
private fun classTarget(session: FirSession, cs: FirClassLikeSymbol<*>): Target? {
    val file = try { cs.fir.moduleData.session.firProvider.getFirClassifierContainerFileIfAny(cs) } catch (e: Exception) { null }
    return declTarget(file?.sourceFile, cs.source, true)
}

private fun isFnType(name: String) = name.startsWith("kotlin/Function") || name.startsWith("kotlin/SuspendFunction") ||
    name.startsWith("kotlin/reflect/KFunction") || name.startsWith("kotlin/reflect/KSuspendFunction")

class KtCallsRegistrar : CompilerPluginRegistrar() {
    override val supportsK2: Boolean = true
    override val pluginId: String = "sieve.ktcalls"
    override fun ExtensionStorage.registerExtensions(configuration: CompilerConfiguration) {
        FirExtensionRegistrarAdapter.registerExtension(KtCallsFir())
    }
}

class KtCallsFir : FirExtensionRegistrar() {
    override fun ExtensionRegistrarContext.configurePlugin() {
        +::KtCallsCheckers
    }
}

class KtCallsCheckers(session: FirSession) : FirAdditionalCheckersExtension(session) {
    override val expressionCheckers: ExpressionCheckers = object : ExpressionCheckers() {
        override val callCheckers: Set<FirExpressionChecker<FirCall>> = setOf(CallChecker)
    }
    override val declarationCheckers: DeclarationCheckers = object : DeclarationCheckers() {
        override val namedFunctionCheckers: Set<FirDeclarationChecker<FirNamedFunction>> = setOf(OverrideChecker, FunDeclChecker)
        override val regularClassCheckers: Set<FirDeclarationChecker<FirRegularClass>> = setOf(ClassDeclChecker)
        override val constructorCheckers: Set<FirDeclarationChecker<FirConstructor>> = setOf(CtorDeclChecker)
        override val propertyCheckers: Set<FirDeclarationChecker<FirProperty>> = setOf(PropDeclChecker)
    }
}

// The owner chain: the named classes and functions around the declaration, innermost last.
private fun ownerChain(context: CheckerContext): String =
    context.containingDeclarations.mapNotNull {
        when (it) {
            is FirRegularClassSymbol -> className(it.fir)
            is FirNamedFunctionSymbol -> it.name.asString()
            else -> null
        }
    }.joinToString(",", "[", "]") { q(it) }

private fun className(c: FirRegularClass): String =
    if (c.isCompanion && c.name.isSpecial) "Companion" else c.name.asString()

private fun emitDecl(context: CheckerContext, src: KtSourceElement?, origin: FirDeclarationOrigin, dkind: String, name: String) {
    val file = context.containingFile ?: return
    if (src == null || src.kind is KtFakeSourceElementKind || origin != FirDeclarationOrigin.Source) return
    val path = rel(file.path ?: return)
    val t = textOf(path)
    val s = skipTrivia(t, src.startOffset)
    val start = lineOf(path, s)
    val end = lineOf(path, maxOf(s, src.endOffset - 1))
    emit("{\"kind\":\"decl\",\"dkind\":${q(dkind)},\"name\":${q(name)},\"owner\":${ownerChain(context)}," +
        "\"file\":${q(path)},\"start\":$start,\"end\":$end}")
}

private object ClassDeclChecker : FirDeclarationChecker<FirRegularClass>(MppCheckerKind.Common) {
    context(context: CheckerContext, reporter: DiagnosticReporter)
    override fun check(declaration: FirRegularClass) {
        try {
            val kind = when {
                declaration.isCompanion -> "companion"
                declaration.classKind == ClassKind.INTERFACE -> "interface"
                declaration.classKind == ClassKind.OBJECT -> "object"
                declaration.classKind == ClassKind.ENUM_CLASS -> "enum"
                declaration.classKind == ClassKind.ENUM_ENTRY -> return
                else -> "class"
            }
            emitDecl(context, declaration.source, declaration.origin, kind, className(declaration))
        } catch (e: Exception) {
            emit("{\"kind\":\"pluginerr\",\"msg\":${q(e.toString().take(200))}}")
        }
    }
}

private object FunDeclChecker : FirDeclarationChecker<FirNamedFunction>(MppCheckerKind.Common) {
    context(context: CheckerContext, reporter: DiagnosticReporter)
    override fun check(declaration: FirNamedFunction) {
        try { emitDecl(context, declaration.source, declaration.origin, "function", declaration.name.asString()) } catch (e: Exception) {
            emit("{\"kind\":\"pluginerr\",\"msg\":${q(e.toString().take(200))}}")
        }
    }
}

private object CtorDeclChecker : FirDeclarationChecker<FirConstructor>(MppCheckerKind.Common) {
    context(context: CheckerContext, reporter: DiagnosticReporter)
    override fun check(declaration: FirConstructor) {
        try { emitDecl(context, declaration.source, declaration.origin, "constructor", "<init>") } catch (e: Exception) {
            emit("{\"kind\":\"pluginerr\",\"msg\":${q(e.toString().take(200))}}")
        }
    }
}

private object PropDeclChecker : FirDeclarationChecker<FirProperty>(MppCheckerKind.Common) {
    context(context: CheckerContext, reporter: DiagnosticReporter)
    override fun check(declaration: FirProperty) {
        try {
            if (declaration.isLocal) return
            emitDecl(context, declaration.source, declaration.origin, "property", declaration.name.asString())
        } catch (e: Exception) {
            emit("{\"kind\":\"pluginerr\",\"msg\":${q(e.toString().take(200))}}")
        }
    }
}

private object OverrideChecker : FirDeclarationChecker<FirNamedFunction>(MppCheckerKind.Common) {
    context(context: CheckerContext, reporter: DiagnosticReporter)
    override fun check(declaration: FirNamedFunction) {
        try { doCheck(declaration) } catch (e: Exception) {
            emit("{\"kind\":\"pluginerr\",\"msg\":${q(e.toString().take(200))}}")
        }
    }

    context(context: CheckerContext)
    private fun doCheck(declaration: FirNamedFunction) {
        if (!declaration.isOverride) return
        val file = context.containingFile ?: return
        val sub = declTarget(file, declaration.source, false) ?: return
        val overridden = with(context) { declaration.symbol.directOverriddenFunctionsSafe() }
        for (b in overridden) {
            val base = b.unwrapFakeOverrides()
            val bf = try { base.fir.moduleData.session.firProvider.getFirCallableContainerFile(base) } catch (e: Exception) { null }
            val bt = declTarget(bf?.sourceFile, base.fir.source, false) ?: continue
            emit("{\"kind\":\"ovrfact\",\"sub\":${q("${sub.file}:${sub.start}")},\"base\":${q("${bt.file}:${bt.start}")}}")
        }
    }
}

private object CallChecker : FirExpressionChecker<FirCall>(MppCheckerKind.Common) {
    context(context: CheckerContext, reporter: DiagnosticReporter)
    override fun check(expression: FirCall) {
        try { doCheck(expression) } catch (e: Exception) {
            emit("{\"kind\":\"pluginerr\",\"msg\":${q(e.toString().take(200))}}")
        }
    }

    context(context: CheckerContext)
    private fun doCheck(expression: FirCall) {
        val file = context.containingFile ?: return
        val session = context.session
        val srcPath = rel(file.path ?: return)
        val ref = when (expression) {
            is FirFunctionCall -> expression.calleeReference
            is FirDelegatedConstructorCall -> expression.calleeReference
            else -> return
        }
        val esrc = expression.source ?: return
        val rsrc = ref.source ?: esrc
        val text = textOf(srcPath)
        // Callee name token: the reference source when it is a real identifier slice.
        val slice = if (rsrc.endOffset <= text.length && rsrc.startOffset < rsrc.endOffset) text.substring(rsrc.startOffset, rsrc.endOffset) else ""
        val ident = Regex("^`?[A-Za-z_][A-Za-z_0-9]*`?").find(slice)?.value?.trim('`')
        val isFake = esrc.kind is KtFakeSourceElementKind
        if (isFake && expression is FirDelegatedConstructorCall) return
        val flags = LinkedHashSet<String>()
        var name: String
        var dst: Target? = null
        var dkey = ""
        val sym: FirCallableSymbol<*>? = if (ref.isError()) null else ref.toResolvedCallableSymbol(false)
        var shape: String
        if (expression is FirDelegatedConstructorCall) {
            shape = "deleg"
            name = if (expression.isThis) "this" else "super"
        } else {
            expression as FirFunctionCall
            val er = expression.explicitReceiver
            shape = when {
                sym is FirConstructorSymbol -> "new"
                er == null -> "bare"
                er is FirThisReceiverExpression -> "this"
                er is FirSuperReceiverExpression -> "super"
                else -> "chain"
            }
            name = ident ?: sym?.name?.asString() ?: "?"
        }
        val line = lineOf(srcPath, skipTrivia(text, rsrc.startOffset))
        if (sym == null) {
            flags.add("err")
        } else {
            val symName = sym.name.asString()
            if (expression is FirFunctionCall && ident != null && ident != symName && symName == "invoke") {
                flags.add("fnval")
            }
            if (expression is FirFunctionCall && ident == null) flags.add("op")
            if (isFake && expression is FirFunctionCall && ident == null) flags.add("op")
            if (sym is FirConstructorSymbol) {
                flags.add("ctor")
                val cid = sym.callableId.classId
                val cs: FirClassLikeSymbol<*>? = when {
                    cid == null -> null
                    cid.isLocal -> sym.resolvedReturnType.toClassSymbol(session)
                    else -> session.symbolProvider.getClassLikeSymbolByClassId(cid)
                }
                if (cs != null) {
                    dst = classTarget(session, cs)
                    if (cs.origin is FirDeclarationOrigin.Java) flags.add("java")
                    else if (dst == null) flags.add("oor")
                }
                if (cs is FirRegularClassSymbol && cs.classKind == ClassKind.ANNOTATION_CLASS) flags.add("annot")
                if (cs is FirRegularClassSymbol && cs.fir.isExpect) flags.add("expect")
            } else {
                val f = sym.unwrapFakeOverrides()
                val fid = f.callableId
                if (fid?.isLocal == true) flags.add("local")
                if (fid != null && fid.classId == null && !fid.isLocal) flags.add("top")
                if (f.fir.receiverParameter != null) flags.add("ext")
                if (fid?.classId?.asString()?.let { isFnType(it) } == true) flags.add("fnval")
                if (f.origin is FirDeclarationOrigin.Java) flags.add("java")
                if (f is FirNamedFunctionSymbol) {
                    val fn = f.fir
                    val ocid = fid?.classId
                    val owner = (if (ocid == null || ocid.isLocal) null else session.symbolProvider.getClassLikeSymbolByClassId(ocid)) as? FirRegularClassSymbol
                    val ck = owner?.classKind
                    if (fn.isExpect) flags.add("expect")
                    if (fn.isOverride) flags.add("ovd")
                    if (fn.isAbstract) flags.add("abs")
                    if ((fn.isOverride || fn.isAbstract || fn.isOpen) && ck != ClassKind.OBJECT && ck != ClassKind.ENUM_ENTRY) flags.add("virt")
                    if (ck == ClassKind.INTERFACE) {
                        flags.add("iface")
                        if (fn.body == null) flags.add("abs")
                    }
                    if ((fn.status.isInfix || fn.status.isOperator) && ident == null) flags.add("op")
                }
                if (f.origin == FirDeclarationOrigin.Source && !flags.contains("java")) {
                    val fileOf = if (fid?.isLocal == true) file else try { f.fir.moduleData.session.firProvider.getFirCallableContainerFile(f)?.sourceFile } catch (e: Exception) { null }
                    dst = declTarget(fileOf, f.fir.source, false)
                    if (dst == null) flags.add("impl")
                } else if (f.origin is FirDeclarationOrigin.Synthetic || f.origin is FirDeclarationOrigin.Plugin) {
                    flags.add("impl")
                } else if (!flags.contains("java")) {
                    flags.add("oor")
                } else {
                    flags.add("oor")
                }
            }
        }
        if (dst != null) dkey = "${dst.file}:${dst.start}"
        val sb = StringBuilder()
        sb.append("{\"src\":").append(q(srcPath)).append(",\"line\":").append(line)
            .append(",\"name\":").append(q(name)).append(",\"shape\":").append(q(shape))
            .append(",\"dst\":").append(q(dst?.file ?: ""))
            .append(",\"dst_start\":").append(dst?.start ?: 0).append(",\"dst_end\":").append(dst?.end ?: 0)
            .append(",\"dkey\":").append(q(dkey))
        for (f in flags) sb.append(",\"").append(f).append("\":true")
        sb.append("}")
        emit(sb.toString())
    }
}
