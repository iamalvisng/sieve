---
name: Config loading
slug: config-loading
type: system
sources:
  - path: src/a.ts
    hash: abc
  - path: src/b.ts
    hash: def
links:
  - to: alpha-headers
    relation: uses
---
## Summary

Alpha parses each config file, then loads the parsed config.
