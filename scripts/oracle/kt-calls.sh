#!/bin/bash
# Oracle runner for Kotlin call targets.
# Usage (run in the repo root): kt-calls.sh <rows.jsonl> <kotlinc args...>
# The kotlinc args are the compile set: -cp <jars>, -d <tmp out dir>, the -Xfragment flags for a
# multiplatform module, and the source files and folders. kotlinc exits non-zero on a compile
# error, and the runner prints "kotlinc exit: N". Rows are in rows.jsonl (see KtCalls.kt).
# Pins: Kotlin 2.4.21 (K2) and Temurin JDK 21.0.12+8, both under ~/.local/share/sieve-oracle/.
set -u
OR="$HOME/.local/share/sieve-oracle"
[ -n "${SIEVE_ORACLE_HOME:-}" ] && OR="$SIEVE_ORACLE_HOME"
export JAVA_HOME="$OR/java/jdk/jdk-21.0.12+8/Contents/Home"
KC="$OR/kotlin/2.4.21/kotlinc"
HERE="$(cd "$(dirname "$0")" && pwd)"
PLUG="$OR/kotlin/ktcalls-plugin.jar"
export JAVA_TOOL_OPTIONS=
export JAVA_OPTS="-Xmx2g"
if [ ! -f "$PLUG" ] || [ "$HERE/KtCalls.kt" -nt "$PLUG" ]; then
  T="$(mktemp -d)"
  "$KC/bin/kotlinc" -nowarn -Xcontext-parameters -cp "$KC/lib/kotlin-compiler.jar" -d "$T/cls" "$HERE/KtCalls.kt" || exit 2
  mkdir -p "$T/cls/META-INF/services"
  echo "sieve.ktcalls.KtCallsRegistrar" > "$T/cls/META-INF/services/org.jetbrains.kotlin.compiler.plugin.CompilerPluginRegistrar"
  "$JAVA_HOME/bin/jar" cf "$PLUG" -C "$T/cls" . || exit 2
  find "$T" -delete
fi
OUT="$1"; shift
: > "$OUT"
export KT_ROOT="$PWD" KT_OUT="$OUT"
# The compile set: every .kt or .java file under a source arg. kt-compare.py reads <rows>.files.
: > "$OUT.files"
for a in "$@"; do
  case "$a" in
    -*) ;;
    *) if [ -d "$a" ]; then find "$a" \( -name '*.kt' -o -name '*.java' \) >> "$OUT.files"
       elif [ -f "$a" ]; then echo "$a" >> "$OUT.files"; fi ;;
  esac
done
"$KC/bin/kotlinc" -nowarn -Xplugin="$PLUG" "$@"
echo "kotlinc exit: $?"
echo "plugin errors: $(grep -c '"kind":"pluginerr"' "$OUT")"
