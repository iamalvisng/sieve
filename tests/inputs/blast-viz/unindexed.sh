# args: blast -d 1 --export-viz vizout
# P1-24, P1-65: no dependents and no symbols. The standard edits are
# committed, and the only change is a markdown file no parser claims, so
# the page holds no node and carries the "no parser claims" empty note.
git commit -q -m edits
printf 'notes\n' >notes.md
git add notes.md
