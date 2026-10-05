# args: blast --format json -d 1
# DV6 (P3-25): two staged files, `alpha` in lib/ and `Zulu` in lib2/. Area
# labels sort with localeCompare: alpha before Zulu.
mkdir -p lib lib2
printf 'export function alpha(): number { return 1; }\n' >lib/alpha.ts
printf 'export function Zulu(): number { return 2; }\n' >lib2/zulu.ts
git add lib lib2
