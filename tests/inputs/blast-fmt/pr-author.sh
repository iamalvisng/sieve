# args: blast --format text -d 1 --pr-author other@example.com
# DV5 (P1-24, P1-25): --pr-author drops "Other Person" by email, so only
# "Third Person" is left to tag.
mkdir -p lib lib2
printf 'import { extra } from "../ts/derived";\nexport function alpha(): number { return extra(); }\n' >lib/alpha.ts
printf 'import { extra } from "../ts/derived";\nexport function Zulu(): number { return extra(); }\n' >lib2/zulu.ts
git add lib
git -c user.name="Other Person" -c user.email=other@example.com commit -q -m alpha -- lib
git add lib2
git -c user.name="Third Person" -c user.email=third@example.com commit -q -m zulu -- lib2
