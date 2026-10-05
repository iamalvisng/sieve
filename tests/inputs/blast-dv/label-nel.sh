# args: blast --format text -d 1
# DV13: JS trim keeps U+0085 in a concept label. Rust trim strips it.
printf -- '---\nname: concept:\302\205Foo Bar\nslug: label-nel\ntype: concept\nsources:\n  - path: ts/derived.ts\n    hash: aaaa\n  - path: ts/newfile.ts\n    hash: aaaa\n---\nbody\n' >sieve/label-nel.md
