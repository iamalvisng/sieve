# args: blast --format markdown -d 1
# DV1, DV5 (P1-24, P1-25, P1-26): the markdown caps. Eight modules of
# eight symbols each: 5 diagram boxes plus the tail circle, 6 table rows
# plus the "smaller areas" row, 60 listed symbols, and 8 owner rows plus
# the "further area" row (1 changed area + 8 modules = 9 rows).
for i in 0 1 2 3 4 5 6 7; do
  mkdir -p "lib$i"
  {
    printf 'import { extra } from "../ts/derived";\n'
    for j in 0 1 2 3 4 5 6 7; do
      printf 'export function lib%sfn%s(): number { return extra(); }\n' "$i" "$j"
    done
  } >"lib$i/callers.ts"
done
git add lib0 lib1 lib2 lib3 lib4 lib5 lib6 lib7
git -c user.name="Other Person" -c user.email=other@example.com commit -q -m callers -- lib0 lib1 lib2 lib3 lib4 lib5 lib6 lib7
