# args: blast
# P3-20: with no --depth, blast uses depth 2. The chain below has three
# callers of the seed `extra`: l1 at depth 1, l2 at depth 2, l3 at depth 3.
# Depth 2 reports l1 and l2 and leaves l3 out.
cat >ts/chain.ts <<'TS'
import { extra } from "./derived";
export function l1(): number { return extra(); }
export function l2(): number { return l1(); }
export function l3(): number { return l2(); }
TS
