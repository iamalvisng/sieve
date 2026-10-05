; Written for Sieve from the tree-sitter-cpp node types.
(struct_specifier name: (type_identifier) @name body: (field_declaration_list)) @definition.class

(class_specifier name: (type_identifier) @name body: (field_declaration_list)) @definition.class

(function_declarator declarator: [(identifier) (field_identifier)] @name) @definition.function

(call_expression function: (identifier) @name) @reference.call

(call_expression function: (qualified_identifier name: (identifier) @name)) @reference.call

(call_expression function: (field_expression field: (field_identifier) @name)) @reference.call
