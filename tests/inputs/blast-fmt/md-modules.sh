# args: blast --format markdown -d 1
# DV1, DV5 (P1-24, P1-25, P1-26): markdown with two changed areas and two
# dependent modules. "Other Person" committed the modules and the second
# area's file, so the tag line, the owner table and the quoted evidence
# lines all appear. The tag line counts areas at 3 or more.
mkdir -p lib lib2 util
printf 'import { extra } from "../ts/derived";\nexport function alpha(): number { return extra(); }\n' >lib/alpha.ts
printf 'import { extra } from "../ts/derived";\nexport function Zulu(): number { return extra(); }\n' >lib2/zulu.ts
printf 'export function helper(): number {\n  return 1;\n}\n' >util/helper.ts
git add lib lib2 util
git -c user.name="Other Person" -c user.email=other@example.com commit -q -m callers -- lib lib2 util
printf 'export function helper(): number {\n  return 2;\n}\n' >util/helper.ts
