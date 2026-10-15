#!/bin/bash
# Resolve a classpath for the Kotlin oracle from a pom in kt-repos/ with the pinned Maven 3.9.16.
# Usage: kt-cp.sh <pom> <scratch dir>   Prints the classpath (colon-separated jars) on stdout.
# The scratch dir holds HOME and the Maven local repo. HOME must not be the real home.
set -eu
OR="$HOME/.local/share/sieve-oracle"
POM="$1"; S="$2"
case "$S" in "$HOME"|"$HOME"/) echo "scratch must not be HOME" >&2; exit 2;; esac
mkdir -p "$S/home" "$S/m2"
export JAVA_HOME="$OR/java/jdk/jdk-21.0.12+8/Contents/Home" MAVEN_OPTS= JAVA_TOOL_OPTIONS=
HOME="$S/home" "$OR/java/maven/apache-maven-3.9.16/bin/mvn" -q -B -f "$POM" -Dmaven.repo.local="$S/m2" \
  org.apache.maven.plugins:maven-dependency-plugin:3.8.1:build-classpath -Dmdep.outputFile="$S/cp.txt" >&2
cat "$S/cp.txt"
