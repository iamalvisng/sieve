# args: blast --format text -d 1
# DV12: JS trim strips U+FEFF from a concept label. Rust trim keeps it.
printf -- '---\nname: concept:\357\273\277Foo Bar\nslug: label-bom\ntype: concept\nsources:\n  - path: ts/derived.ts\n    hash: aaaa\n  - path: ts/newfile.ts\n    hash: aaaa\n---\nbody\n' >sieve/label-bom.md
