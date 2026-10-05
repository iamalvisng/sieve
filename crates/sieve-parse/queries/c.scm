; Written for Sieve from the tree-sitter-c node types.
(struct_specifier name: (type_identifier) @name body: (field_declaration_list)) @definition.class

(function_declarator declarator: (identifier) @name) @definition.function

(call_expression function: (identifier) @name) @reference.call

(enum_specifier name: (type_identifier) @name body: (enumerator_list)) @definition.type

(type_definition declarator: (type_identifier) @name) @definition.type
