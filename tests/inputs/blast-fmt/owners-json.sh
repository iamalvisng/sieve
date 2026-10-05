# args: blast --format json -d 1
# DV5 (P1-25, P3-25): the same second author, as JSON: `owners` on the
# module and `reviewers` with `areas`. The author date is pinned so
# `last` is byte-stable. `score` decays with the clock (120-day
# half-life from now), so the test masks it. The pinned date leaves the
# 36-month history window on 2029-09-01; move the pin before then.
mkdir -p lib
printf 'import { extra } from "../ts/derived";\nexport function alpha(): number { return extra(); }\n' >lib/alpha.ts
git add lib
GIT_AUTHOR_DATE=2026-09-01T08:00:00+08:00 GIT_COMMITTER_DATE=2026-09-01T08:00:00+08:00 \
  git -c user.name="Other Person" -c user.email=other@example.com commit -q -m callers -- lib
