# args: blast -d 1 --export-viz vizout --title PR-7
# P1-24, P1-65: two changed areas and two dependent modules, as the
# md-modules case. "Other Person" committed the modules and the second
# area's file, so the nodes carry owners. The author date is pinned so
# the owner `last` day in the page is byte-stable. The pinned date leaves
# the 36-month history window on 2029-09-01; move the pin before then.
# `--title` sets the page subtitle.
mkdir -p lib lib2 util
printf 'import { extra } from "../ts/derived";\nexport function alpha(): number { return extra(); }\n' >lib/alpha.ts
printf 'import { extra } from "../ts/derived";\nexport function Zulu(): number { return extra(); }\n' >lib2/zulu.ts
printf 'export function helper(): number {\n  return 1;\n}\n' >util/helper.ts
git add lib lib2 util
GIT_AUTHOR_DATE=2026-09-01T08:00:00+08:00 GIT_COMMITTER_DATE=2026-09-01T08:00:00+08:00 \
  git -c user.name="Other Person" -c user.email=other@example.com commit -q -m callers -- lib lib2 util
printf 'export function helper(): number {\n  return 2;\n}\n' >util/helper.ts
