# args: blast --format mermaid -d 1
# DV1 (P1-24): mermaid with two dependent modules, alpha and Zulu.
mkdir -p lib lib2
printf 'import { extra } from "../ts/derived";\nexport function alpha(): number { return extra(); }\n' >lib/alpha.ts
printf 'import { extra } from "../ts/derived";\nexport function Zulu(): number { return extra(); }\n' >lib2/zulu.ts
git add lib lib2
git -c user.name="Other Person" -c user.email=other@example.com commit -q -m callers -- lib lib2
