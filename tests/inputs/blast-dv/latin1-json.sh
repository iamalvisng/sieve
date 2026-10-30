# args: blast --format json -d 1
# A changed file with the byte 0xE9 (latin1, not UTF-8).
printf '\nexport function latin(): string { return "caf\351"; }\n' >>ts/derived.ts
git add ts/derived.ts
