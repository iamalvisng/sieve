; Written for Sieve from the tree-sitter-clojure node types.
(list_lit
  . (sym_lit name: (sym_name) @head)
  . (sym_lit name: (sym_name) @name)
  (#match? @head "^(defn|defn-|defmacro|defmulti)$")) @definition.function

(list_lit
  . (sym_lit name: (sym_name) @head)
  . (sym_lit name: (sym_name) @name)
  (#match? @head "^(defrecord|deftype)$")) @definition.class

(list_lit
  . (sym_lit name: (sym_name) @head)
  . (sym_lit name: (sym_name) @name)
  (#eq? @head "defprotocol")) @definition.interface

(list_lit
  . (sym_lit name: (sym_name) @head)
  . (sym_lit name: (sym_name) @name)
  (#match? @head "^(def|defonce)$")) @definition.variable

(list_lit
  . (sym_lit name: (sym_name) @name)
  (#not-match? @name "^def")) @reference.call
