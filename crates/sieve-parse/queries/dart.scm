; Written for Sieve from the tree-sitter-dart node types.
(class_declaration name: (identifier) @name) @definition.class

(function_declaration
  signature: (function_signature name: (identifier) @name)) @definition.function

(method_signature
  (function_signature name: (identifier) @name)) @definition.method

(mixin_declaration name: (identifier) @name) @definition.class

(enum_declaration name: (identifier) @name) @definition.enum

(extension_declaration name: (identifier) @name) @definition.class

(call_expression function: (identifier) @name) @reference.call
