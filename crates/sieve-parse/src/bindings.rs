//! Type bindings for one file: a pre-order scan that maps a name in a
//! scope to the type it was constructed or annotated with. Extraction
//! uses this map to resolve a `this.x()` or `self.x()` call's receiver
//! type. Every rule follows the binding notes; see
//! the `edges-ts-py.md` note section 4.

use std::collections::HashMap;

use tree_sitter::Node as TsNode;

use sieve_core::Kind;

use crate::extract::{describe, swift_user_type_last_identifier};

/// One file's type bindings: a scoped name-to-type map, plus the import
/// alias map every recorded type name passes through.
pub struct FileBindings {
    /// Key `"<scope.join(\".\")>|<name>"`, value the bound type name.
    pub map: HashMap<String, String>,
    /// Local alias name to the real name it stands for.
    pub aliases: HashMap<String, String>,
}

impl FileBindings {
    /// Looks up `name` in `scope`, trying the innermost scope first, then
    /// each shorter prefix, down to the file-level empty scope.
    pub fn lookup(&self, scope: &[String], name: &str) -> Option<&str> {
        for depth in (0..=scope.len()).rev() {
            let prefix = scope[..depth].join(".");
            let key = format!("{prefix}|{name}");
            if let Some(type_name) = self.map.get(&key) {
                return Some(type_name.as_str());
            }
        }
        None
    }

    /// Resolves a call receiver text (such as `self`, `this.other`, or a
    /// bare variable name) to a bound type name.
    pub fn resolve_recv_type(
        &self,
        receiver: &str,
        scope: &[String],
        enclosing_class: Option<&str>,
    ) -> Option<String> {
        match receiver {
            "self" | "cls" | "this" => return enclosing_class.map(str::to_string),
            "super" => return None,
            _ => {}
        }
        if let Some(rest) = receiver.strip_prefix("this.") {
            if let Some(hit) = self.lookup(scope, receiver) {
                return Some(hit.to_string());
            }
            let rewritten = format!("self.{rest}");
            return self.lookup(scope, &rewritten).map(str::to_string);
        }
        self.lookup(scope, receiver).map(str::to_string)
    }
}

/// The scope stack this scan carries at each node: the current scope, and
/// the scope of the nearest enclosing class (an attribute binds there,
/// even from inside a method).
struct BCtx {
    scope: Vec<String>,
    enclosing_kind: Option<Kind>,
    class_scope: Vec<String>,
}

/// Collects every type binding in one parsed file, plus its import aliases.
///
/// This runs as its own pre-order pass, before extraction, so a binding
/// declared later in the file is visible to an earlier call site.
pub fn collect_bindings(tree_root: TsNode, source: &str, grammar: &str) -> FileBindings {
    // R has no binding collector (edges note, section 1.6): its call
    // resolution never consults a typed-receiver table.
    if grammar == "r" {
        return FileBindings {
            map: HashMap::new(),
            aliases: HashMap::new(),
        };
    }
    let is_python = grammar == "python";
    let aliases = collect_aliases(tree_root, source, is_python);
    let mut map = HashMap::new();
    let root_ctx = BCtx {
        scope: Vec::new(),
        enclosing_kind: None,
        class_scope: Vec::new(),
    };
    let mut cursor = tree_root.walk();
    for child in tree_root.named_children(&mut cursor) {
        walk_bindings(child, &root_ctx, source, grammar, &aliases, &mut map);
    }
    FileBindings { map, aliases }
}

/// Scans the whole tree for an import alias: Python `aliased_import`, or a
/// TS `import_specifier` with an `alias` field.
fn collect_aliases(root: TsNode, source: &str, is_python: bool) -> HashMap<String, String> {
    let mut aliases = HashMap::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let is_alias_node = (is_python && node.kind() == "aliased_import")
            || (!is_python && node.kind() == "import_specifier");
        let alias_pair = if is_alias_node {
            pair_text(node, "name", "alias", source)
        } else {
            None
        };
        if let Some((real, alias)) = alias_pair {
            aliases.insert(alias, real);
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    aliases
}

/// Reads two field children as text, in `(field_a, field_b)` order.
fn pair_text(node: TsNode, field_a: &str, field_b: &str, source: &str) -> Option<(String, String)> {
    let a = node
        .child_by_field_name(field_a)?
        .utf8_text(source.as_bytes())
        .ok()?;
    let b = node
        .child_by_field_name(field_b)?
        .utf8_text(source.as_bytes())
        .ok()?;
    Some((a.to_string(), b.to_string()))
}

/// Walks one node in pre-order. A definition node pushes its own scope
/// segment for its children, matching `extract::walk`. Any other node is
/// checked for a binding, then walked with the same scope.
fn walk_bindings(
    node: TsNode,
    ctx: &BCtx,
    source: &str,
    grammar: &str,
    aliases: &HashMap<String, String>,
    map: &mut HashMap<String, String>,
) {
    let enclosing_class = ctx.class_scope.last().map(String::as_str);
    if let Some(desc) = describe(node, ctx.enclosing_kind, enclosing_class, source, grammar) {
        let mut child_scope = ctx.scope.clone();
        child_scope.push(desc.name);
        let child_class_scope = if desc.kind == Kind::Class {
            child_scope.clone()
        } else {
            ctx.class_scope.clone()
        };
        let child_ctx = BCtx {
            scope: child_scope,
            enclosing_kind: Some(desc.kind),
            class_scope: child_class_scope,
        };
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            walk_bindings(child, &child_ctx, source, grammar, aliases, map);
        }
        return;
    }

    match grammar {
        "python" => match node.kind() {
            "typed_parameter" => bind_py_typed_parameter(node, source, ctx, aliases, map),
            "assignment" => bind_py_assignment(node, source, ctx, aliases, map),
            _ => {}
        },
        "go" => match node.kind() {
            "short_var_declaration" => bind_go_short_var(node, source, ctx, aliases, map),
            "var_spec" => bind_go_var_spec(node, source, ctx, aliases, map),
            _ => {}
        },
        "java" => match node.kind() {
            "local_variable_declaration" => bind_java_local(node, source, ctx, aliases, map),
            "formal_parameter" => bind_java_formal_parameter(node, source, ctx, aliases, map),
            "field_declaration" => bind_java_field(node, source, ctx, aliases, map),
            _ => {}
        },
        "php" => match node.kind() {
            "simple_parameter" => bind_php_simple_parameter(node, source, ctx, aliases, map),
            "assignment_expression" => bind_php_assignment(node, source, ctx, aliases, map),
            _ => {}
        },
        "swift" => match node.kind() {
            "parameter" => bind_swift_parameter(node, source, ctx, aliases, map),
            "property_declaration" => bind_swift_property(node, source, ctx, aliases, map),
            _ => {}
        },
        _ => match node.kind() {
            "variable_declarator" => bind_ts_variable_declarator(node, source, ctx, aliases, map),
            "public_field_definition" => bind_ts_public_field(node, source, ctx, aliases, map),
            "required_parameter" => bind_ts_required_parameter(node, source, ctx, aliases, map),
            _ => {}
        },
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk_bindings(child, ctx, source, grammar, aliases, map);
    }
}

/// Inserts `type_name`, passed through `aliases`, at `scope|key_name`.
fn bind(
    map: &mut HashMap<String, String>,
    aliases: &HashMap<String, String>,
    scope: &[String],
    key_name: &str,
    type_name: &str,
) {
    let resolved = aliases
        .get(type_name)
        .cloned()
        .unwrap_or_else(|| type_name.to_string());
    let key = format!("{}|{key_name}", scope.join("."));
    map.insert(key, resolved);
}

/// Returns the first `type_identifier` text inside a TS `type_annotation`.
fn ts_type_identifier(ann: Option<TsNode>, source: &str) -> Option<String> {
    let ann = ann?;
    let mut cursor = ann.walk();
    let found = ann
        .named_children(&mut cursor)
        .find(|c| c.kind() == "type_identifier")
        .and_then(|c| c.utf8_text(source.as_bytes()).ok())
        .map(str::to_string);
    found
}

/// Returns the `new Foo()` constructor name, when `value` is a
/// `new_expression`.
fn ts_new_constructor(value: TsNode, source: &str) -> Option<String> {
    if value.kind() != "new_expression" {
        return None;
    }
    value
        .child_by_field_name("constructor")?
        .utf8_text(source.as_bytes())
        .ok()
        .map(str::to_string)
}

/// `variable_declarator`: `new Foo()` first, then a type annotation. Key:
/// the current scope, bare name.
fn bind_ts_variable_declarator(
    node: TsNode,
    source: &str,
    ctx: &BCtx,
    aliases: &HashMap<String, String>,
    map: &mut HashMap<String, String>,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let Ok(name) = name_node.utf8_text(source.as_bytes()) else {
        return;
    };
    if let Some(type_name) = node
        .child_by_field_name("value")
        .and_then(|v| ts_new_constructor(v, source))
        .or_else(|| ts_type_identifier(node.child_by_field_name("type"), source))
    {
        bind(map, aliases, &ctx.scope, name, &type_name);
    }
}

/// `public_field_definition`: an annotation first, then `new Foo()`. Key:
/// the class scope, name `this.<field>`.
fn bind_ts_public_field(
    node: TsNode,
    source: &str,
    ctx: &BCtx,
    aliases: &HashMap<String, String>,
    map: &mut HashMap<String, String>,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let Ok(name) = name_node.utf8_text(source.as_bytes()) else {
        return;
    };
    let type_name = ts_type_identifier(node.child_by_field_name("type"), source).or_else(|| {
        node.child_by_field_name("value")
            .and_then(|v| ts_new_constructor(v, source))
    });
    if let Some(type_name) = type_name {
        let key = format!("this.{name}");
        bind(map, aliases, &ctx.class_scope, &key, &type_name);
    }
}

/// `required_parameter`: annotation only. Key: the current scope, bare
/// name. A parameter property also binds `this.<name>` at the class scope.
fn bind_ts_required_parameter(
    node: TsNode,
    source: &str,
    ctx: &BCtx,
    aliases: &HashMap<String, String>,
    map: &mut HashMap<String, String>,
) {
    let Some(pattern) = node.child_by_field_name("pattern") else {
        return;
    };
    if pattern.kind() != "identifier" {
        return;
    }
    let Ok(name) = pattern.utf8_text(source.as_bytes()) else {
        return;
    };
    let Some(type_name) = ts_type_identifier(node.child_by_field_name("type"), source) else {
        return;
    };
    bind(map, aliases, &ctx.scope, name, &type_name);

    let mut cursor = node.walk();
    let is_param_property = node
        .children(&mut cursor)
        .any(|c| matches!(c.kind(), "accessibility_modifier" | "readonly" | "override"));
    if is_param_property {
        let key = format!("this.{name}");
        bind(map, aliases, &ctx.class_scope, &key, &type_name);
    }
}

/// `short_var_declaration` (`g := Greeter{...}`): zips `left`'s and
/// `right`'s expression lists, and binds each identifier whose paired
/// value names a type.
fn bind_go_short_var(
    node: TsNode,
    source: &str,
    ctx: &BCtx,
    aliases: &HashMap<String, String>,
    map: &mut HashMap<String, String>,
) {
    let Some(left) = node.child_by_field_name("left") else {
        return;
    };
    let Some(right) = node.child_by_field_name("right") else {
        return;
    };
    let mut lc = left.walk();
    let mut rc = right.walk();
    for (name_node, value_node) in left
        .named_children(&mut lc)
        .zip(right.named_children(&mut rc))
    {
        if name_node.kind() != "identifier" {
            continue;
        }
        let Ok(name) = name_node.utf8_text(source.as_bytes()) else {
            continue;
        };
        if let Some(type_name) = go_value_type_name(value_node, source) {
            bind(map, aliases, &ctx.scope, name, &type_name);
        }
    }
}

/// `var_spec` (`var g Greeter`, or `var g = Greeter{...}`): the explicit
/// `type` field wins; else the `value` field's type.
fn bind_go_var_spec(
    node: TsNode,
    source: &str,
    ctx: &BCtx,
    aliases: &HashMap<String, String>,
    map: &mut HashMap<String, String>,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let Ok(name) = name_node.utf8_text(source.as_bytes()) else {
        return;
    };
    let type_name = node
        .child_by_field_name("type")
        .filter(|t| t.kind() == "type_identifier")
        .and_then(|t| t.utf8_text(source.as_bytes()).ok())
        .map(str::to_string)
        .or_else(|| {
            node.child_by_field_name("value")
                .and_then(|v| go_value_type_name(v, source))
        });
    if let Some(type_name) = type_name {
        bind(map, aliases, &ctx.scope, name, &type_name);
    }
}

/// Names the type a Go value expression constructs: a composite literal's
/// own type, one `&` unwrapped, or the `NewX()` convention's `X`
/// (edges note, section 1.1).
fn go_value_type_name(value: TsNode, source: &str) -> Option<String> {
    match value.kind() {
        "composite_literal" => {
            let type_node = value.child_by_field_name("type")?;
            if type_node.kind() != "type_identifier" {
                return None;
            }
            type_node
                .utf8_text(source.as_bytes())
                .ok()
                .map(str::to_string)
        }
        "unary_expression" => {
            let operator = value.child_by_field_name("operator")?;
            if operator.utf8_text(source.as_bytes()).ok() != Some("&") {
                return None;
            }
            let operand = value.child_by_field_name("operand")?;
            go_value_type_name(operand, source)
        }
        "call_expression" => {
            let func = value.child_by_field_name("function")?;
            if func.kind() != "identifier" {
                return None;
            }
            let name = func.utf8_text(source.as_bytes()).ok()?;
            let rest = name.strip_prefix("New")?;
            if rest.is_empty() {
                return None;
            }
            Some(rest.to_string())
        }
        _ => None,
    }
}

/// `local_variable_declaration` (`App app = new App(...)`): the `type`
/// field's own name, else the declarator's `new` value.
fn bind_java_local(
    node: TsNode,
    source: &str,
    ctx: &BCtx,
    aliases: &HashMap<String, String>,
    map: &mut HashMap<String, String>,
) {
    let Some(declarator) = node.child_by_field_name("declarator") else {
        return;
    };
    let Some(name_node) = declarator.child_by_field_name("name") else {
        return;
    };
    let Ok(name) = name_node.utf8_text(source.as_bytes()) else {
        return;
    };
    let type_name = node
        .child_by_field_name("type")
        .filter(|t| t.kind() == "type_identifier")
        .and_then(|t| t.utf8_text(source.as_bytes()).ok())
        .map(str::to_string)
        .or_else(|| {
            declarator
                .child_by_field_name("value")
                .filter(|v| v.kind() == "object_creation_expression")
                .and_then(|v| v.child_by_field_name("type"))
                .and_then(|t| t.utf8_text(source.as_bytes()).ok())
                .map(str::to_string)
        });
    if let Some(type_name) = type_name {
        bind(map, aliases, &ctx.scope, name, &type_name);
    }
}

/// `formal_parameter` (`Greeter g`): the `type` field's own name.
fn bind_java_formal_parameter(
    node: TsNode,
    source: &str,
    ctx: &BCtx,
    aliases: &HashMap<String, String>,
    map: &mut HashMap<String, String>,
) {
    let Some(type_node) = node.child_by_field_name("type") else {
        return;
    };
    if type_node.kind() != "type_identifier" {
        return;
    }
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let (Ok(type_name), Ok(name)) = (
        type_node.utf8_text(source.as_bytes()),
        name_node.utf8_text(source.as_bytes()),
    ) else {
        return;
    };
    bind(map, aliases, &ctx.scope, name, type_name);
}

/// `field_declaration` (`private Greeter g;`): binds twice, bare and
/// `this.`-prefixed, at the class scope (edges note, section 1.2).
fn bind_java_field(
    node: TsNode,
    source: &str,
    ctx: &BCtx,
    aliases: &HashMap<String, String>,
    map: &mut HashMap<String, String>,
) {
    let Some(type_node) = node.child_by_field_name("type") else {
        return;
    };
    if type_node.kind() != "type_identifier" {
        return;
    }
    let Some(declarator) = node.child_by_field_name("declarator") else {
        return;
    };
    let Some(name_node) = declarator.child_by_field_name("name") else {
        return;
    };
    let (Ok(type_name), Ok(name)) = (
        type_node.utf8_text(source.as_bytes()),
        name_node.utf8_text(source.as_bytes()),
    ) else {
        return;
    };
    bind(map, aliases, &ctx.class_scope, name, type_name);
    let key = format!("this.{name}");
    bind(map, aliases, &ctx.class_scope, &key, type_name);
}

/// Returns the type name of a PHP type node: a bare `name`, a
/// dequalified `qualified_name`, or `None` for a primitive or optional
/// type this task does not resolve.
fn php_type_name(type_node: TsNode, source: &str) -> Option<String> {
    match type_node.kind() {
        "name" => type_node
            .utf8_text(source.as_bytes())
            .ok()
            .map(str::to_string),
        "qualified_name" => {
            let text = type_node.utf8_text(source.as_bytes()).ok()?;
            Some(text.rsplit('\\').next().unwrap_or(text).to_string())
        }
        _ => None,
    }
}

/// `simple_parameter` (`App $app`): the `type` field's own name. Keys
/// keep the `$` (edges note, section 1.5).
fn bind_php_simple_parameter(
    node: TsNode,
    source: &str,
    ctx: &BCtx,
    aliases: &HashMap<String, String>,
    map: &mut HashMap<String, String>,
) {
    let Some(type_name) = node
        .child_by_field_name("type")
        .and_then(|t| php_type_name(t, source))
    else {
        return;
    };
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let Ok(name) = name_node.utf8_text(source.as_bytes()) else {
        return;
    };
    bind(map, aliases, &ctx.scope, name, &type_name);
}

/// Returns the class name a PHP `object_creation_expression` constructs:
/// its first named child that is not the `arguments` list, when it is a
/// bare or qualified name.
fn php_new_type_name(node: TsNode, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    let name_node = node
        .named_children(&mut cursor)
        .find(|c| c.kind() != "arguments")?;
    php_type_name(name_node, source)
}

/// `$x = new Foo()`: binds the left variable to the constructed type.
/// Keys keep the `$` (edges note, section 1.5).
fn bind_php_assignment(
    node: TsNode,
    source: &str,
    ctx: &BCtx,
    aliases: &HashMap<String, String>,
    map: &mut HashMap<String, String>,
) {
    let Some(left) = node.child_by_field_name("left") else {
        return;
    };
    if left.kind() != "variable_name" {
        return;
    }
    let Some(right) = node.child_by_field_name("right") else {
        return;
    };
    if right.kind() != "object_creation_expression" {
        return;
    }
    let Some(type_name) = php_new_type_name(right, source) else {
        return;
    };
    let Ok(name) = left.utf8_text(source.as_bytes()) else {
        return;
    };
    bind(map, aliases, &ctx.scope, name, &type_name);
}

/// Returns the first named child's text of a Python `type` field node.
fn py_type_name(type_field: TsNode, source: &str) -> Option<String> {
    type_field
        .named_child(0)
        .and_then(|c| c.utf8_text(source.as_bytes()).ok())
        .map(str::to_string)
}

/// Returns the callee name of a bare Python `call`, else `None`.
fn call_type_name(right: TsNode, source: &str) -> Option<String> {
    if right.kind() != "call" {
        return None;
    }
    let func = right.child_by_field_name("function")?;
    if func.kind() != "identifier" {
        return None;
    }
    func.utf8_text(source.as_bytes()).ok().map(str::to_string)
}

/// `typed_parameter`: the `type` field. Key: the current scope, bare name.
fn bind_py_typed_parameter(
    node: TsNode,
    source: &str,
    ctx: &BCtx,
    aliases: &HashMap<String, String>,
    map: &mut HashMap<String, String>,
) {
    let Some(name_node) = node.named_child(0) else {
        return;
    };
    let Ok(name) = name_node.utf8_text(source.as_bytes()) else {
        return;
    };
    let Some(type_name) = node
        .child_by_field_name("type")
        .and_then(|t| py_type_name(t, source))
    else {
        return;
    };
    bind(map, aliases, &ctx.scope, name, &type_name);
}

/// `assignment`: an identifier left side binds the type field, else the
/// callee of a bare call. A `self.`/`cls.` attribute left side binds the
/// callee of a bare call at the class scope, name `self.<attr>`.
fn bind_py_assignment(
    node: TsNode,
    source: &str,
    ctx: &BCtx,
    aliases: &HashMap<String, String>,
    map: &mut HashMap<String, String>,
) {
    let Some(left) = node.child_by_field_name("left") else {
        return;
    };
    match left.kind() {
        "identifier" => {
            let Ok(name) = left.utf8_text(source.as_bytes()) else {
                return;
            };
            let type_name = node
                .child_by_field_name("type")
                .and_then(|t| py_type_name(t, source))
                .or_else(|| {
                    node.child_by_field_name("right")
                        .and_then(|r| call_type_name(r, source))
                });
            if let Some(type_name) = type_name {
                bind(map, aliases, &ctx.scope, name, &type_name);
            }
        }
        "attribute" => {
            let Some(object) = left.child_by_field_name("object") else {
                return;
            };
            let Ok(object_text) = object.utf8_text(source.as_bytes()) else {
                return;
            };
            if object.kind() != "identifier" || !matches!(object_text, "self" | "cls") {
                return;
            }
            let Some(attr) = left.child_by_field_name("attribute") else {
                return;
            };
            let Ok(attr_name) = attr.utf8_text(source.as_bytes()) else {
                return;
            };
            let type_name = node
                .child_by_field_name("right")
                .and_then(|r| call_type_name(r, source));
            if let Some(type_name) = type_name {
                let key = format!("self.{attr_name}");
                bind(map, aliases, &ctx.class_scope, &key, &type_name);
            }
        }
        _ => {}
    }
}

/// A Swift `parameter`'s local name: the LAST direct `simple_identifier`
/// child (an external label plus an internal name both parse as
/// `simple_identifier`; a single-name parameter has just one). Key: the
/// current scope.
fn bind_swift_parameter(
    node: TsNode,
    source: &str,
    ctx: &BCtx,
    aliases: &HashMap<String, String>,
    map: &mut HashMap<String, String>,
) {
    let mut cursor = node.walk();
    let idents: Vec<TsNode> = node
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "simple_identifier")
        .collect();
    let Some(name_node) = idents.last() else {
        return;
    };
    let Ok(name) = name_node.utf8_text(source.as_bytes()) else {
        return;
    };
    let Some(type_name) = swift_user_type_last_identifier(node, source) else {
        return;
    };
    bind(map, aliases, &ctx.scope, name, &type_name);
}

/// A Swift `property_declaration`'s bound name: its `type_annotation`
/// first, else an UpperCamelCase initializer call (`let app = App(...)`).
/// A property whose parent is `class_body`/`protocol_body` binds at the
/// class scope, bare and `self.`-prefixed; any other property (a local
/// `let`/`var`) binds at the current scope.
fn bind_swift_property(
    node: TsNode,
    source: &str,
    ctx: &BCtx,
    aliases: &HashMap<String, String>,
    map: &mut HashMap<String, String>,
) {
    let Some(pattern) = node.child_by_field_name("name") else {
        return;
    };
    let mut cursor = pattern.walk();
    let Some(ident) = pattern
        .named_children(&mut cursor)
        .find(|c| c.kind() == "simple_identifier")
    else {
        return;
    };
    let Ok(name) = ident.utf8_text(source.as_bytes()) else {
        return;
    };
    let type_name = swift_type_annotation_identifier(node, source).or_else(|| {
        node.child_by_field_name("value")
            .and_then(|v| swift_upper_camel_call_name(v, source))
    });
    let Some(type_name) = type_name else {
        return;
    };
    let in_type_body = matches!(
        node.parent().map(|p| p.kind()),
        Some("class_body") | Some("protocol_body")
    );
    if in_type_body {
        bind(map, aliases, &ctx.class_scope, name, &type_name);
        let key = format!("self.{name}");
        bind(map, aliases, &ctx.class_scope, &key, &type_name);
    } else {
        bind(map, aliases, &ctx.scope, name, &type_name);
    }
}

/// A `property_declaration`'s `type_annotation` child, reduced to its
/// last `type_identifier` (`swift_user_type_last_identifier`).
fn swift_type_annotation_identifier(node: TsNode, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    let ann = node
        .named_children(&mut cursor)
        .find(|c| c.kind() == "type_annotation")?;
    swift_user_type_last_identifier(ann, source)
}

/// Names the type an UpperCamelCase initializer call constructs
/// (`Text("hi")`, `App(name: "sieve")`): the callee, when it is a bare
/// identifier starting with an uppercase letter.
fn swift_upper_camel_call_name(value: TsNode, source: &str) -> Option<String> {
    if value.kind() != "call_expression" {
        return None;
    }
    let mut cursor = value.walk();
    let target = value.named_children(&mut cursor).next()?;
    if target.kind() != "simple_identifier" {
        return None;
    }
    let text = target.utf8_text(source.as_bytes()).ok()?;
    text.chars()
        .next()
        .is_some_and(char::is_uppercase)
        .then(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn ts_bindings(source: &str) -> FileBindings {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
            .expect("set typescript language");
        let tree = parser.parse(source, None).expect("parse");
        collect_bindings(tree.root_node(), source, "typescript")
    }

    fn py_bindings(source: &str) -> FileBindings {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .expect("set python language");
        let tree = parser.parse(source, None).expect("parse");
        collect_bindings(tree.root_node(), source, "python")
    }

    #[test]
    fn ts_const_new_binds_the_constructor_name_under_the_function_scope() {
        let bindings =
            ts_bindings("function main(): void {\n  const app = new App(\"sieve\");\n}\n");
        assert_eq!(bindings.lookup(&["main".to_string()], "app"), Some("App"));
    }

    #[test]
    fn py_bare_assignment_binds_the_bare_call_callee() {
        let bindings = py_bindings("def run():\n    calc = Calculator()\n");
        assert_eq!(
            bindings.lookup(&["run".to_string()], "calc"),
            Some("Calculator")
        );
    }

    #[test]
    fn py_self_attribute_binds_at_the_class_scope() {
        let bindings = py_bindings("class C:\n    def m(self):\n        self.attr = Foo()\n");
        assert_eq!(
            bindings.lookup(&["C".to_string(), "m".to_string()], "self.attr"),
            Some("Foo")
        );
        assert_eq!(
            bindings.lookup(&["C".to_string()], "self.attr"),
            Some("Foo")
        );
    }

    #[test]
    fn ts_import_alias_maps_the_bound_type_name() {
        let bindings = ts_bindings(
            "import { Model as M } from \"./model\";\nfunction f() {\n  const x = new M();\n}\n",
        );
        assert_eq!(bindings.lookup(&["f".to_string()], "x"), Some("Model"));
    }

    #[test]
    fn lookup_tries_the_innermost_scope_first() {
        let mut map = HashMap::new();
        map.insert("a.b|x".to_string(), "Inner".to_string());
        map.insert("a|x".to_string(), "Outer".to_string());
        let bindings = FileBindings {
            map,
            aliases: HashMap::new(),
        };
        let scope = vec!["a".to_string(), "b".to_string()];
        assert_eq!(bindings.lookup(&scope, "x"), Some("Inner"));
        assert_eq!(bindings.lookup(&scope[..1], "x"), Some("Outer"));
    }

    #[test]
    fn resolve_recv_type_this_returns_the_enclosing_class() {
        let bindings = FileBindings {
            map: HashMap::new(),
            aliases: HashMap::new(),
        };
        assert_eq!(
            bindings.resolve_recv_type("this", &[], Some("App")),
            Some("App".to_string())
        );
        assert_eq!(bindings.resolve_recv_type("super", &[], Some("App")), None);
    }
}
