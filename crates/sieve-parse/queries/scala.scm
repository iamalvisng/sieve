; Written for Sieve from the tree-sitter-scala node types.
(class_definition name: (identifier) @name) @definition.class

(object_definition name: (identifier) @name) @definition.class

(trait_definition name: (identifier) @name) @definition.interface

(function_definition name: (identifier) @name) @definition.function

(val_definition pattern: (identifier) @name) @definition.variable

(class_parameter name: (identifier) @name) @definition.variable

(extends_clause type: (type_identifier) @name) @reference.class

(call_expression function: (identifier) @name) @reference.call
