; Written for Sieve from the tree-sitter-nix node types.
(binding
  attrpath: (attrpath . attr: (identifier) @name .)
  expression: (function_expression) @definition.function)

(apply_expression
  function: (variable_expression name: (identifier) @name)) @reference.call

(apply_expression function: (select_expression attrpath: (attrpath attr: (identifier) @name .))) @reference.call
