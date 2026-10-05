# args: blast --format json --no-owners -d 1
# DV5 (P1-24, P1-25): --no-owners with a second author present. No area
# or module carries `owners`, and `reviewers` is absent.
mkdir -p lib
printf 'import { extra } from "../ts/derived";\nexport function alpha(): number { return extra(); }\n' >lib/alpha.ts
git add lib
git -c user.name="Other Person" -c user.email=other@example.com commit -q -m callers -- lib
