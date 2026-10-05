# args: blast -d all
# P3-22: blast follows in edges only. The seed `newFn` calls `leaf`, an
# out edge. The seed `extra` has the caller chain l1, l2, l3, an in edge.
# The report lists the callers and never lists `leaf`.
cat >ts/chain.ts <<'TS'
import { extra } from "./derived";
export function l1(): number { return extra(); }
export function l2(): number { return l1(); }
export function l3(): number { return l2(); }
TS
cat >ts/leaf.ts <<'TS'
export function leaf(): number { return 7; }
TS
cat >ts/newfile.ts <<'TS'
import { leaf } from "./leaf";
export function newFn(): number {
  return leaf();
}
TS
