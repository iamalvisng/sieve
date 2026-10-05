; Written for Sieve from the tree-sitter-c-sharp node types.
(namespace_declaration name: (identifier) @name) @definition.module

(class_declaration name: (identifier) @name) @definition.class

(interface_declaration name: (identifier) @name) @definition.interface

(method_declaration name: (identifier) @name) @definition.method

(constructor_declaration name: (identifier) @name) @definition.method

(enum_declaration name: (identifier) @name) @definition.enum

(record_declaration name: (identifier) @name) @definition.class

(property_declaration name: (identifier) @name) @definition.variable

(invocation_expression function: (identifier) @name) @reference.call


