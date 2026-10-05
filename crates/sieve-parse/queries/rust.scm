; Written for Sieve from the tree-sitter-rust node types.
(struct_item name: (type_identifier) @name) @definition.struct

(enum_item name: (type_identifier) @name) @definition.enum

(const_item name: (identifier) @name) @definition.constant

(static_item name: (identifier) @name) @definition.constant

(function_item name: (identifier) @name) @definition.function

(function_signature_item name: (identifier) @name) @definition.function

(trait_item name: (type_identifier) @name) @definition.interface

(mod_item name: (identifier) @name) @definition.module

(call_expression function: (identifier) @name) @reference.call

(call_expression function: (scoped_identifier name: (identifier) @name)) @reference.call

(type_item name: (type_identifier) @name) @definition.type

(macro_definition name: (identifier) @name) @definition.function

(macro_invocation macro: (identifier) @name) @reference.call
