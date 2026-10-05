---
name: Calculator Application
slug: calculator-application
type: system
sources:
  - path: py/main.py
    hash: 2ed82f71c10b311d3d0a67b0f9cfa6be7db8c8f00f8b3d4b8c04e4af7bccd880
sources_digest: 51cb4e7923db48cfce174e836132078289e0b35f605563ba2c635e90d086ca93
links:
  - to: basic-arithmetic-helpers
    relation: uses
    description: >-
      The Calculator class uses the add function from the Basic Arithmetic
      Helpers to perform addition.
generator:
  version: 1
covers:
  - symbol: Calculator
    kind: class
    at: 'py/main.py:L4-L6'
  - symbol: total
    kind: method
    at: 'py/main.py:L5-L6'
  - symbol: run
    kind: function
    at: 'py/main.py:L9-L11'
---
<!-- context:generated:start -->
## Summary

The main entry point for a simple calculator application, this module encapsulates the addition functionality within a Calculator class, demonstrating modular design by utilizing helper functions for arithmetic operations.

## Related

- uses [[basic-arithmetic-helpers]] — The Calculator class uses the add function from the Basic Arithmetic Helpers to perform addition.
<!-- context:generated:end -->

## Notes

_Anything written below the generated block is preserved when the graph is regenerated._
