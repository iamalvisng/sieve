# args: blast --format text -d 1
# DV5 (P1-25, P3-25): a second author on a dependent module. The text
# report ends with a "who to tag" block naming "Other Person". The commit
# is made at run time, so the block reads "last today" on every run.
mkdir -p lib
printf 'import { extra } from "../ts/derived";\nexport function alpha(): number { return extra(); }\n' >lib/alpha.ts
git add lib
git -c user.name="Other Person" -c user.email=other@example.com commit -q -m callers -- lib
