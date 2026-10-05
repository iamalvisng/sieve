# args: blast --depth 0
# DV4 (P1-27): on a dirty tree the refresh note comes before the flag error.
# The edit below makes the graph stale, so the refresh runs.
printf '\nexport const dirty = 1;\n' >>ts/newfile.ts
