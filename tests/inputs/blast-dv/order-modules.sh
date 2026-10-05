# args: blast --format text -d 1
# DV6 (P3-25): two committed callers of `extra`, `alpha` in lib/ and `Zulu`
# in lib2/. Module labels sort with localeCompare: alpha before Zulu.
mkdir -p lib lib2
printf 'import { extra } from "../ts/derived";\nexport function alpha(): number { return extra(); }\n' >lib/alpha.ts
printf 'import { extra } from "../ts/derived";\nexport function Zulu(): number { return extra(); }\n' >lib2/zulu.ts
git add lib lib2
git -c user.name=sieve-fixture -c user.email=fixture@sieve.local commit -q -m callers -- lib lib2
