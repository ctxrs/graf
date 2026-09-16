use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use tree_sitter::{Node as Syntax, Parser};

use crate::model::*;

pub(crate) const MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;

pub(crate) fn empty_facts(path: &str, hash: &str) -> FileFacts {
    let module = path.strip_prefix("src/").unwrap_or(path);
    let module = module.strip_suffix(".py").unwrap_or(module);
    let module = module.strip_suffix("/__init__").unwrap_or(module);
    let module = if module == "__init__" { "" } else { module };
    FileFacts {
        path: path.into(),
        hash: hash.into(),
        module: module.replace('/', "."),
        nodes: vec![],
        edges: vec![],
        references: vec![],
        diagnostics: vec![],
    }
}

#[derive(Clone)]
enum Binding {
    Definition {
        key: String,
        id: String,
        start: usize,
    },
    Symbol {
        key: String,
        start: usize,
    },
    Module {
        module: String,
        prefix: String,
        start: usize,
    },
    Unknown,
}

#[derive(Clone, Copy, PartialEq)]
enum ScopeKind {
    Module,
    Class,
    Function,
    Opaque,
}

struct Scope {
    parent: Option<usize>,
    kind: ScopeKind,
    owner: String,
    qualified: String,
    bindings: HashMap<String, Binding>,
    uncertain: bool,
}

struct PendingCall {
    scope: usize,
    start: usize,
    end: usize,
    line: u32,
    parts: Vec<String>,
}

struct Extractor<'a> {
    source: &'a str,
    facts: FileFacts,
    scopes: Vec<Scope>,
    calls: Vec<PendingCall>,
}

/// Extract static Python facts without executing source or guessing dynamic targets.
pub fn parse_python(path: &str, source: &str, hash: &str) -> Result<FileFacts> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
        || !path.ends_with(".py")
    {
        bail!("Python source path must be a normalized relative POSIX .py path");
    }
    let mut facts = empty_facts(path, hash);
    if source.len() > MAX_SOURCE_BYTES {
        facts.diagnostics.push(Diagnostic {
            file: path.into(),
            line: None,
            message: "Python source exceeds the 4 MiB limit".into(),
        });
        return Ok(facts);
    }
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_python::LANGUAGE.into())?;
    let tree = parser
        .parse(source, None)
        .context("Python parser did not return a tree")?;
    let root = tree.root_node();
    // Bound our traversal as well as input size; generated deeply nested syntax is not useful here.
    let mut pending = vec![(root, 0)];
    while let Some((node, depth)) = pending.pop() {
        if node.is_error() || node.is_missing() || depth > 256 {
            facts.diagnostics.push(Diagnostic {
                file: path.into(),
                line: Some(line(node)),
                message: if depth > 256 {
                    "Python syntax nesting exceeds the indexing limit"
                } else {
                    "Python syntax error; no facts indexed for this file"
                }
                .into(),
            });
            return Ok(facts);
        }
        let mut cursor = node.walk();
        pending.extend(node.children(&mut cursor).map(|n| (n, depth + 1)));
    }
    let owner = format!("python:{path}:module");
    facts.nodes.push(Node {
        id: owner.clone(),
        label: facts.module.clone(),
        kind: "module".into(),
        file: path.into(),
        line: Some(1),
        end_line: Some(line_end(root)),
        qualified_name: Some(facts.module.clone()),
        binding_key: Some(format!("module:{}", facts.module)),
        metadata: Value::Null,
    });
    let mut extractor = Extractor {
        source,
        facts,
        calls: vec![],
        scopes: vec![Scope {
            parent: None,
            kind: ScopeKind::Module,
            owner,
            qualified: String::new(),
            bindings: HashMap::new(),
            uncertain: false,
        }],
    };
    extractor.visit(root, 0, false);
    extractor.finish();
    Ok(extractor.facts)
}

fn line(node: Syntax<'_>) -> u32 {
    node.start_position().row as u32 + 1
}
fn line_end(node: Syntax<'_>) -> u32 {
    let end = node.end_position();
    (end.row + usize::from(end.column != 0 || end.row == 0)) as u32
}

impl Extractor<'_> {
    fn text(&self, node: Syntax<'_>) -> &str {
        &self.source[node.byte_range()]
    }

    fn bind(&mut self, scope: usize, name: String, binding: Binding) {
        self.scopes[scope]
            .bindings
            .entry(name)
            .and_modify(|b| *b = Binding::Unknown)
            .or_insert(binding);
    }

    fn target(&mut self, node: Syntax<'_>, scope: usize) {
        match node.kind() {
            "identifier" => self.bind(scope, self.text(node).into(), Binding::Unknown),
            "attribute" => {
                if let Some(object) = node.child_by_field_name("object") {
                    self.target(object, scope);
                }
            }
            "subscript" => {
                if let Some(value) = node.child_by_field_name("value") {
                    self.target(value, scope);
                }
            }
            _ => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    self.target(child, scope);
                }
            }
        }
    }

    fn parameters(&mut self, node: Syntax<'_>, scope: usize) {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            match child.kind() {
                "default_parameter" | "typed_default_parameter" => {
                    if let Some(name) = child.child_by_field_name("name") {
                        self.target(name, scope);
                    }
                }
                "typed_parameter" => {
                    if let Some(name) = child.named_child(0) {
                        self.target(name, scope);
                    }
                }
                _ => self.target(child, scope),
            }
        }
    }

    fn visit(&mut self, node: Syntax<'_>, scope: usize, conditional: bool) {
        match node.kind() {
            "function_definition" | "class_definition" => {
                self.definition(node, scope, conditional);
                return;
            }
            "import_statement" | "import_from_statement" => {
                self.import(node, scope, conditional);
                return;
            }
            "lambda"
            | "list_comprehension"
            | "set_comprehension"
            | "dictionary_comprehension"
            | "generator_expression" => {
                // ponytail: anonymous scopes retain call sites, but do not infer their bindings.
                let child = self.scopes.len();
                self.scopes.push(Scope {
                    parent: Some(scope),
                    kind: ScopeKind::Opaque,
                    owner: self.scopes[scope].owner.clone(),
                    qualified: self.scopes[scope].qualified.clone(),
                    bindings: HashMap::new(),
                    uncertain: true,
                });
                let mut cursor = node.walk();
                for n in node.named_children(&mut cursor) {
                    self.visit(n, child, true);
                }
                return;
            }
            "assignment"
            | "augmented_assignment"
            | "type_alias_statement"
            | "for_statement"
            | "for_in_clause" => {
                if let Some(target) = node.child_by_field_name("left") {
                    self.target(target, scope);
                }
            }
            "named_expression" => {
                if let Some(target) = node.child_by_field_name("name") {
                    self.target(target, scope);
                    if self.scopes[scope].kind == ScopeKind::Opaque {
                        let mut parent = self.scopes[scope].parent;
                        while let Some(index) = parent {
                            self.target(target, index);
                            if self.scopes[index].kind != ScopeKind::Opaque {
                                break;
                            }
                            parent = self.scopes[index].parent;
                        }
                    }
                }
            }
            "as_pattern" | "except_clause" => {
                if let Some(target) = node.child_by_field_name("alias") {
                    self.target(target, scope);
                }
            }
            "delete_statement" | "global_statement" | "nonlocal_statement" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    self.target(child, scope);
                }
                // A declaration can allow writes to an enclosing binding. Do not export a guessed target.
                if node.kind() != "delete_statement" {
                    let mut parent = self.scopes[scope].parent;
                    while let Some(index) = parent {
                        let mut cursor = node.walk();
                        for child in node.named_children(&mut cursor) {
                            self.target(child, index);
                        }
                        parent = self.scopes[index].parent;
                    }
                }
            }
            "case_clause" => self.scopes[scope].uncertain = true,
            "exec_statement" => self.scopes[scope].uncertain = true,
            "call" => {
                let parts = node
                    .child_by_field_name("function")
                    .and_then(|n| self.dotted(n))
                    .unwrap_or_default();
                if parts.len() == 1 && matches!(parts[0].as_str(), "exec" | "globals" | "locals") {
                    self.scopes[scope].uncertain = true;
                    if parts[0] == "globals" {
                        self.scopes[0].uncertain = true;
                    }
                }
                self.calls.push(PendingCall {
                    scope,
                    start: node.start_byte(),
                    end: node.end_byte(),
                    line: line(node),
                    parts,
                });
            }
            _ => {}
        }
        let conditional = conditional
            || matches!(
                node.kind(),
                "if_statement"
                    | "for_statement"
                    | "while_statement"
                    | "try_statement"
                    | "with_statement"
                    | "match_statement"
                    | "decorated_definition"
            );
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.visit(child, scope, conditional);
        }
    }

    fn definition(&mut self, node: Syntax<'_>, scope: usize, conditional: bool) {
        let name = self
            .text(node.child_by_field_name("name").unwrap())
            .to_owned();
        let qualified = if self.scopes[scope].qualified.is_empty() {
            name.clone()
        } else {
            format!("{}.{}", self.scopes[scope].qualified, name)
        };
        let id = format!(
            "python:{}:{}@{}",
            self.facts.path,
            qualified,
            node.start_byte()
        );
        let key = format!("python:{}:{qualified}", self.facts.module);
        let class = node.kind() == "class_definition";
        self.bind(
            scope,
            name.clone(),
            if conditional {
                Binding::Unknown
            } else {
                Binding::Definition {
                    key: key.clone(),
                    id: id.clone(),
                    start: node.end_byte(),
                }
            },
        );
        self.facts.nodes.push(Node {
            id: id.clone(),
            label: name,
            kind: if class {
                "class"
            } else if self.scopes[scope].kind == ScopeKind::Class {
                "method"
            } else {
                "function"
            }
            .into(),
            file: self.facts.path.clone(),
            line: Some(line(node)),
            end_line: Some(line_end(node)),
            qualified_name: Some(qualified.clone()),
            binding_key: Some(key),
            metadata: Value::Null,
        });
        self.facts.edges.push(Edge {
            id: format!("contains:{id}"),
            source: self.scopes[scope].owner.clone(),
            target: id.clone(),
            relation: "contains".into(),
            directed: true,
            file: Some(self.facts.path.clone()),
            line: Some(line(node)),
            confidence: "static".into(),
            metadata: Value::Null,
        });
        let child = self.scopes.len();
        self.scopes.push(Scope {
            parent: Some(scope),
            kind: if class {
                ScopeKind::Class
            } else {
                ScopeKind::Function
            },
            owner: id,
            qualified,
            bindings: HashMap::new(),
            uncertain: false,
        });
        if node.child_by_field_name("type_parameters").is_some() {
            self.scopes[child].uncertain = true;
        }
        if let Some(parameters) = node.child_by_field_name("parameters") {
            self.parameters(parameters, child);
        }
        let body = node.child_by_field_name("body").unwrap();
        let mut cursor = node.walk();
        for n in node.named_children(&mut cursor) {
            if n.id() == body.id() {
                self.visit(n, child, false);
            } else {
                self.visit(n, scope, conditional);
            }
        }
    }

    fn relative_module(&self, name: &str) -> Option<String> {
        let dots = name.bytes().take_while(|c| *c == b'.').count();
        if dots == 0 {
            return Some(name.into());
        }
        let mut package: Vec<_> = self
            .facts
            .module
            .split('.')
            .filter(|p| !p.is_empty())
            .collect();
        if !self.facts.path.ends_with("/__init__.py") && self.facts.path != "__init__.py" {
            package.pop();
        }
        if dots > package.len() {
            return None;
        }
        package.truncate(package.len() - dots + 1);
        if !name[dots..].is_empty() {
            package.push(&name[dots..]);
        }
        Some(package.join("."))
    }

    fn import(&mut self, node: Syntax<'_>, scope: usize, conditional: bool) {
        let from = node.kind() == "import_from_statement";
        let module = node
            .child_by_field_name("module_name")
            .and_then(|n| self.relative_module(self.text(n)));
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() == "wildcard_import" {
                self.scopes[scope].uncertain = true;
                self.import_reference(
                    child,
                    scope,
                    "*".into(),
                    vec![],
                    "star import is not resolved",
                );
            }
        }
        let mut cursor = node.walk();
        for item in node.children_by_field_name("name", &mut cursor) {
            let name_node = item.child_by_field_name("name").unwrap_or(item);
            let name = self.text(name_node).to_owned();
            let alias = item
                .child_by_field_name("alias")
                .map(|n| self.text(n).to_owned());
            let local = alias.clone().unwrap_or_else(|| {
                if from {
                    name.clone()
                } else {
                    name.split('.').next().unwrap().into()
                }
            });
            let (binding, keys) = if from {
                match &module {
                    Some(module) => {
                        let key = format!("python:{module}:{name}");
                        (
                            Binding::Symbol {
                                key: key.clone(),
                                start: node.end_byte(),
                            },
                            vec![key, format!("module:{module}.{name}")],
                        )
                    }
                    None => (Binding::Unknown, vec![]),
                }
            } else {
                (
                    Binding::Module {
                        module: name.clone(),
                        prefix: alias.unwrap_or_else(|| name.clone()),
                        start: node.end_byte(),
                    },
                    vec![format!("module:{name}")],
                )
            };
            self.bind(
                scope,
                local,
                if conditional {
                    Binding::Unknown
                } else {
                    binding
                },
            );
            let label = module
                .as_ref()
                .map_or_else(|| name.clone(), |m| format!("{m}.{name}"));
            self.import_reference(
                item,
                scope,
                label,
                keys,
                "import target is unavailable or ambiguous",
            );
        }
    }

    fn import_reference(
        &mut self,
        node: Syntax<'_>,
        scope: usize,
        label: String,
        keys: Vec<String>,
        reason: &str,
    ) {
        self.facts.references.push(Reference {
            id: format!(
                "import:{}:{}-{}",
                self.scopes[scope].owner,
                node.start_byte(),
                node.end_byte()
            ),
            source: self.scopes[scope].owner.clone(),
            label,
            relation: "imports".into(),
            file: self.facts.path.clone(),
            line: line(node),
            candidate_keys: keys,
            reason: reason.into(),
        });
    }

    fn dotted(&self, node: Syntax<'_>) -> Option<Vec<String>> {
        match node.kind() {
            "identifier" => Some(vec![self.text(node).into()]),
            "attribute" => {
                let mut parts = self.dotted(node.child_by_field_name("object")?)?;
                parts.push(self.text(node.child_by_field_name("attribute")?).into());
                Some(parts)
            }
            "parenthesized_expression" if node.named_child_count() == 1 => {
                self.dotted(node.named_child(0)?)
            }
            _ => None,
        }
    }

    fn call_key(&self, call: &PendingCall) -> Option<String> {
        let name = call.parts.first()?;
        let mut index = Some(call.scope);
        let mut deferred = false;
        while let Some(current) = index {
            let scope = &self.scopes[current];
            // Class namespaces are not lexical closures, including for nested classes.
            if current == call.scope || scope.kind != ScopeKind::Class {
                if scope.uncertain {
                    return None;
                }
                if let Some(binding) = scope.bindings.get(name) {
                    return match binding {
                        Binding::Definition { key, start, .. } | Binding::Symbol { key, start }
                            if call.parts.len() == 1 && (deferred || *start <= call.start) =>
                        {
                            Some(key.clone())
                        }
                        Binding::Module {
                            module,
                            prefix,
                            start,
                        } if deferred || *start <= call.start => {
                            let dotted = call.parts.join(".");
                            let suffix = dotted.strip_prefix(&format!("{prefix}."))?;
                            if suffix.contains('.') {
                                None
                            } else {
                                Some(format!("python:{module}:{suffix}"))
                            }
                        }
                        _ => None,
                    };
                }
            }
            deferred |= scope.kind == ScopeKind::Function;
            index = scope.parent;
        }
        None
    }

    fn finish(&mut self) {
        let valid: HashMap<_, _> = self
            .scopes
            .iter()
            .filter(|s| !s.uncertain)
            .flat_map(|s| s.bindings.values())
            .filter_map(|b| {
                if let Binding::Definition { id, key, .. } = b {
                    Some((id.clone(), key.clone()))
                } else {
                    None
                }
            })
            .collect();
        for node in self.facts.nodes.iter_mut().filter(|n| n.kind != "module") {
            node.binding_key = valid.get(&node.id).cloned();
        }
        for call in &self.calls {
            let key = self.call_key(call);
            let reason = if key.is_some() {
                "static target is unavailable or ambiguous"
            } else {
                "dynamic, shadowed, or uncertain Python binding"
            };
            self.facts.references.push(Reference {
                id: format!(
                    "call:{}:{}-{}",
                    self.scopes[call.scope].owner, call.start, call.end
                ),
                source: self.scopes[call.scope].owner.clone(),
                label: if call.parts.is_empty() {
                    "<dynamic call>".into()
                } else {
                    call.parts.join(".")
                },
                relation: "calls".into(),
                file: self.facts.path.clone(),
                line: call.line,
                candidate_keys: key.into_iter().collect(),
                reason: reason.into(),
            });
        }
    }
}
