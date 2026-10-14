; Written for Sieve from the tree-sitter-c-sharp node types.
(namespace_declaration name: (identifier) @name) @definition.module

(class_declaration name: (identifier) @name) @definition.class

(interface_declaration name: (identifier) @name) @definition.interface

(method_declaration name: (identifier) @name) @definition.method

(constructor_declaration name: (identifier) @name) @definition.method

(enum_declaration name: (identifier) @name) @definition.enum

(record_declaration name: (identifier) @name) @definition.class

(property_declaration name: (identifier) @name) @definition.variable

; Calls come from `cs_proof.rs`, which proves each call or leaves it Inferred.


