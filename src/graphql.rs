//! GraphQL over HTTP: the request body, the introspection query and its schema, a ready-made
//! operation for a root field, light checks and formatting of a query. The HTTP call is the app's.

use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashSet};

/// `{"query", "variables", "operationName"}`; empty variables / operation name are left out.
pub fn request_body(query: &str, variables: &str, operation_name: &str) -> Result<String, String> {
    let mut body = Map::new();
    body.insert("query".into(), Value::String(query.to_string()));
    if !variables.trim().is_empty() {
        let vars: Value = serde_json::from_str(variables)
            .map_err(|e| format!("The variables aren't valid JSON: {} (line {}, column {})", strip_position(&e.to_string()), e.line(), e.column()))?;
        match vars {
            Value::Object(_) => {
                body.insert("variables".into(), vars);
            }
            Value::Null => {}
            other => return Err(format!("The variables must be a JSON object ({{\"name\": value}}), not {}", kind_of(&other))),
        }
    }
    if !operation_name.trim().is_empty() {
        body.insert("operationName".into(), Value::String(operation_name.trim().to_string()));
    }
    Ok(Value::Object(body).to_string())
}

fn strip_position(msg: &str) -> &str {
    msg.find(" at line ").map_or(msg, |i| &msg[..i])
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

/// The standard full introspection query (as graphql-js `getIntrospectionQuery()` with descriptions).
pub const INTROSPECTION_QUERY: &str = r#"query IntrospectionQuery {
  __schema {
    queryType { name }
    mutationType { name }
    subscriptionType { name }
    types {
      ...FullType
    }
    directives {
      name
      description
      locations
      args {
        ...InputValue
      }
    }
  }
}

fragment FullType on __Type {
  kind
  name
  description
  fields(includeDeprecated: true) {
    name
    description
    args {
      ...InputValue
    }
    type {
      ...TypeRef
    }
    isDeprecated
    deprecationReason
  }
  inputFields {
    ...InputValue
  }
  interfaces {
    ...TypeRef
  }
  enumValues(includeDeprecated: true) {
    name
    description
    isDeprecated
    deprecationReason
  }
  possibleTypes {
    ...TypeRef
  }
}

fragment InputValue on __InputValue {
  name
  description
  type {
    ...TypeRef
  }
  defaultValue
}

fragment TypeRef on __Type {
  kind
  name
  ofType {
    kind
    name
    ofType {
      kind
      name
      ofType {
        kind
        name
        ofType {
          kind
          name
          ofType {
            kind
            name
            ofType {
              kind
              name
              ofType {
                kind
                name
              }
            }
          }
        }
      }
    }
  }
}
"#;

#[derive(Clone, Debug, Default)]
pub struct Schema {
    pub query_type: Option<String>,
    pub mutation_type: Option<String>,
    pub subscription_type: Option<String>,
    pub types: Vec<TypeInfo>,
}

#[derive(Clone, Debug)]
pub struct TypeInfo {
    pub name: String,
    /// `OBJECT` `INTERFACE` `UNION` `ENUM` `INPUT_OBJECT` `SCALAR`.
    pub kind: String,
    pub description: String,
    pub fields: Vec<FieldInfo>,
    pub input_fields: Vec<ArgInfo>,
    pub enum_values: Vec<String>,
    pub possible_types: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct FieldInfo {
    pub name: String,
    pub description: String,
    pub args: Vec<ArgInfo>,
    /// As written in GraphQL: `[User!]!`.
    pub type_ref: String,
    /// The named type inside: `User`.
    pub base_type: String,
    pub deprecated: bool,
}

#[derive(Clone, Debug)]
pub struct ArgInfo {
    pub name: String,
    pub type_ref: String,
    /// The default value as GraphQL text (`10`, `"x"`, `ASC`).
    pub default: Option<String>,
    pub description: String,
}

/// Reads an introspection response: `{"data": {"__schema": …}}` or `{"__schema": …}`.
pub fn parse_introspection(response_body: &str) -> Result<Schema, String> {
    let v: Value = serde_json::from_str(response_body).map_err(|e| format!("The response isn't JSON: {e}"))?;
    let schema = v.pointer("/data/__schema").or_else(|| v.get("__schema")).filter(|s| s.is_object());
    let Some(s) = schema else {
        let errors: Vec<String> = v
            .get("errors")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|e| e.get("message").and_then(Value::as_str).map_or_else(|| e.to_string(), str::to_string)).collect())
            .unwrap_or_default();
        if !errors.is_empty() {
            return Err(format!("The server answered with errors: {}", errors.join("; ")));
        }
        return Err("No __schema in the response (is introspection turned off on this server?)".into());
    };
    let root = |k: &str| s.get(k).and_then(|t| t.get("name")).and_then(Value::as_str).map(str::to_string);
    let types = s.get("types").and_then(Value::as_array).map(|a| a.iter().filter_map(parse_type).collect()).unwrap_or_default();
    Ok(Schema { query_type: root("queryType"), mutation_type: root("mutationType"), subscription_type: root("subscriptionType"), types })
}

fn text(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

fn list<'a>(v: &'a Value, k: &str) -> impl Iterator<Item = &'a Value> {
    v.get(k).and_then(Value::as_array).into_iter().flatten()
}

fn parse_type(t: &Value) -> Option<TypeInfo> {
    let name = t.get("name")?.as_str()?.to_string();
    Some(TypeInfo {
        name,
        kind: text(t, "kind"),
        description: text(t, "description"),
        fields: list(t, "fields").filter_map(parse_field).collect(),
        input_fields: list(t, "inputFields").filter_map(parse_arg).collect(),
        enum_values: list(t, "enumValues").filter_map(|e| e.get("name")?.as_str().map(str::to_string)).collect(),
        possible_types: list(t, "possibleTypes").filter_map(|p| p.get("name")?.as_str().map(str::to_string)).collect(),
    })
}

fn parse_field(f: &Value) -> Option<FieldInfo> {
    let (type_ref, base_type) = type_ref(f.get("type")?);
    Some(FieldInfo {
        name: f.get("name")?.as_str()?.to_string(),
        description: text(f, "description"),
        args: list(f, "args").filter_map(parse_arg).collect(),
        type_ref,
        base_type,
        deprecated: f.get("isDeprecated").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn parse_arg(a: &Value) -> Option<ArgInfo> {
    Some(ArgInfo {
        name: a.get("name")?.as_str()?.to_string(),
        type_ref: type_ref(a.get("type")?).0,
        default: a.get("defaultValue").and_then(Value::as_str).map(str::to_string),
        description: text(a, "description"),
    })
}

/// `{kind: NON_NULL, ofType: {kind: LIST, …}}` → (`[User!]!`, `User`).
fn type_ref(t: &Value) -> (String, String) {
    match t.get("kind").and_then(Value::as_str) {
        Some("NON_NULL") => {
            let (inner, base) = t.get("ofType").map(type_ref).unwrap_or_default();
            (format!("{inner}!"), base)
        }
        Some("LIST") => {
            let (inner, base) = t.get("ofType").map(type_ref).unwrap_or_default();
            (format!("[{inner}]"), base)
        }
        _ => {
            let name = text(t, "name");
            (name.clone(), name)
        }
    }
}

impl Schema {
    pub fn get(&self, name: &str) -> Option<&TypeInfo> {
        self.types.iter().find(|t| t.name == name)
    }

    /// The fields of the `query`, `mutation` or `subscription` root type.
    pub fn root_fields(&self, op: &str) -> Vec<&FieldInfo> {
        self.root_type(op).map(|t| t.fields.iter().collect()).unwrap_or_default()
    }

    fn root_type(&self, op: &str) -> Option<&TypeInfo> {
        let name = match op {
            "query" => self.query_type.as_deref(),
            "mutation" => self.mutation_type.as_deref(),
            "subscription" => self.subscription_type.as_deref(),
            _ => None,
        }?;
        self.get(name)
    }

    fn is_leaf(&self, name: &str) -> bool {
        self.get(name).is_none_or(|t| t.kind == "SCALAR" || t.kind == "ENUM")
    }
}

/// A ready-to-edit operation for one root field: its arguments as variables ($id: ID!), and a selection of scalar fields (nested objects expanded `depth` levels, __typename for unions), plus a variables JSON skeleton.
pub fn operation_for(schema: &Schema, op: &str, field: &str, depth: usize) -> Option<(String, String)> {
    let f = schema.root_type(op)?.fields.iter().find(|f| f.name == field)?;
    let mut q = format!("{op} {}", operation_name(field));
    if !f.args.is_empty() {
        let decls: Vec<String> = f
            .args
            .iter()
            .map(|a| match &a.default {
                Some(d) => format!("${}: {} = {d}", a.name, a.type_ref),
                None => format!("${}: {}", a.name, a.type_ref),
            })
            .collect();
        q.push_str(&format!("({})", decls.join(", ")));
    }
    q.push_str(" {\n  ");
    q.push_str(&f.name);
    if !f.args.is_empty() {
        let uses: Vec<String> = f.args.iter().map(|a| format!("{0}: ${0}", a.name)).collect();
        q.push_str(&format!("({})", uses.join(", ")));
    }
    if !schema.is_leaf(&f.base_type) {
        let mut path = vec![f.base_type.clone()];
        let lines = selection(schema, &f.base_type, depth, &mut path);
        q.push_str(" {\n");
        for l in lines {
            q.push_str("    ");
            q.push_str(&l);
            q.push('\n');
        }
        q.push_str("  }");
    }
    q.push_str("\n}\n");

    let mut vars = Map::new();
    for a in &f.args {
        let value = a.default.as_deref().and_then(|d| serde_json::from_str(d).ok()).unwrap_or_else(|| placeholder(schema, &a.type_ref, 3));
        vars.insert(a.name.clone(), value);
    }
    let vars = serde_json::to_string_pretty(&Value::Object(vars)).unwrap_or_else(|_| "{}".into());
    Some((q, vars))
}

/// `users` → `Users`, `create_user` → `CreateUser`.
fn operation_name(field: &str) -> String {
    field
        .split('_')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut c = p.chars();
            c.next().map(|f| f.to_uppercase().chain(c).collect::<String>()).unwrap_or_default()
        })
        .collect()
}

/// The lines of a selection set for `type_name` (two spaces per nested level). `path` avoids cycles.
fn selection(schema: &Schema, type_name: &str, depth: usize, path: &mut Vec<String>) -> Vec<String> {
    let Some(t) = schema.get(type_name) else { return vec!["__typename".into()] };
    let mut out = Vec::new();
    if t.kind == "UNION" || (t.kind == "INTERFACE" && !t.possible_types.is_empty()) {
        out.push("__typename".into());
    }
    for f in &t.fields {
        // A field with a required argument can't be selected without a value.
        if f.args.iter().any(|a| a.type_ref.ends_with('!') && a.default.is_none()) {
            continue;
        }
        if schema.is_leaf(&f.base_type) {
            out.push(f.name.clone());
        } else if depth > 0 && !path.contains(&f.base_type) {
            path.push(f.base_type.clone());
            let inner = selection(schema, &f.base_type, depth - 1, path);
            path.pop();
            if inner.iter().any(|l| l != "__typename") {
                out.push(format!("{} {{", f.name));
                out.extend(inner.into_iter().map(|l| format!("  {l}")));
                out.push("}".into());
            }
        }
    }
    if t.kind == "UNION" && depth > 0 {
        for p in &t.possible_types {
            if path.contains(p) {
                continue;
            }
            path.push(p.clone());
            let inner = selection(schema, p, depth - 1, path);
            path.pop();
            if inner.iter().any(|l| l != "__typename") {
                out.push(format!("... on {p} {{"));
                out.extend(inner.into_iter().filter(|l| l != "__typename").map(|l| format!("  {l}")));
                out.push("}".into());
            }
        }
    }
    if out.is_empty() {
        out.push("__typename".into());
    }
    out
}

/// A value of the right shape to start from: `0`, `""`, `false`, the first enum value, an object for an input type.
fn placeholder(schema: &Schema, type_ref: &str, depth: usize) -> Value {
    let t = type_ref.trim_end_matches('!');
    if t.starts_with('[') {
        return json!([]);
    }
    match t {
        "Int" => json!(0),
        "Float" => json!(0.0),
        "Boolean" => json!(false),
        "String" | "ID" => json!(""),
        _ => match schema.get(t) {
            Some(info) if info.kind == "ENUM" => info.enum_values.first().map_or(Value::Null, |v| json!(v)),
            Some(info) if info.kind == "INPUT_OBJECT" && depth > 0 => {
                let mut m = Map::new();
                for f in &info.input_fields {
                    let v = f.default.as_deref().and_then(|d| serde_json::from_str(d).ok()).unwrap_or_else(|| placeholder(schema, &f.type_ref, depth - 1));
                    m.insert(f.name.clone(), v);
                }
                Value::Object(m)
            }
            Some(info) if info.kind == "INPUT_OBJECT" => json!({}),
            _ => json!(""),
        },
    }
}

// ---- lexer (shared by `lint` and `format_query`) ----

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    /// `{ } ( ) [ ] : = ! | & , ...`
    Punct(&'static str),
    Name(String),
    /// `$name`
    Var(String),
    /// `@name`
    Directive(String),
    /// A string, block string or number, as written.
    Value(String),
    Comment(String),
    /// An unterminated string (kept as written).
    BadString(String),
    Other(char),
}

struct Token {
    tok: Tok,
    line: usize,
}

fn lex(src: &str) -> Vec<Token> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let (mut i, mut line) = (0, 1);
    let name_char = |c: char| c == '_' || c.is_ascii_alphanumeric();
    let take_name = |i: &mut usize| {
        let start = *i;
        while *i < chars.len() && name_char(chars[*i]) {
            *i += 1;
        }
        chars[start..*i].iter().collect::<String>()
    };
    while i < chars.len() {
        let c = chars[i];
        let start_line = line;
        match c {
            '\n' => {
                line += 1;
                i += 1;
            }
            c if c.is_whitespace() || c == '\u{feff}' => i += 1,
            '#' => {
                let start = i;
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                out.push(Token { tok: Tok::Comment(chars[start..i].iter().collect::<String>().trim_end().to_string()), line: start_line });
            }
            '"' if chars[i..].starts_with(&['"', '"', '"']) => {
                let start = i;
                i += 3;
                let mut closed = false;
                while i < chars.len() {
                    if chars[i] == '\\' && chars[i + 1..].starts_with(&['"', '"', '"']) {
                        i += 4;
                    } else if chars[i..].starts_with(&['"', '"', '"']) {
                        i += 3;
                        closed = true;
                        break;
                    } else {
                        if chars[i] == '\n' {
                            line += 1;
                        }
                        i += 1;
                    }
                }
                let s: String = chars[start..i].iter().collect();
                out.push(Token { tok: if closed { Tok::Value(s) } else { Tok::BadString(s) }, line: start_line });
            }
            '"' => {
                let start = i;
                i += 1;
                let mut closed = false;
                while i < chars.len() && chars[i] != '\n' {
                    if chars[i] == '\\' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    if chars[i - 1] == '"' {
                        closed = true;
                        break;
                    }
                }
                let i2 = i.min(chars.len());
                i = i2;
                let s: String = chars[start..i].iter().collect();
                out.push(Token { tok: if closed { Tok::Value(s) } else { Tok::BadString(s) }, line: start_line });
            }
            '.' if chars[i..].starts_with(&['.', '.', '.']) => {
                i += 3;
                out.push(Token { tok: Tok::Punct("..."), line });
            }
            '$' => {
                i += 1;
                out.push(Token { tok: Tok::Var(take_name(&mut i)), line });
            }
            '@' => {
                i += 1;
                out.push(Token { tok: Tok::Directive(take_name(&mut i)), line });
            }
            c if c == '-' || c.is_ascii_digit() => {
                let start = i;
                i += 1;
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '.' | '+' | '-')) {
                    i += 1;
                }
                out.push(Token { tok: Tok::Value(chars[start..i].iter().collect()), line });
            }
            c if c == '_' || c.is_ascii_alphabetic() => out.push(Token { tok: Tok::Name(take_name(&mut i)), line }),
            _ => {
                let p = match c {
                    '{' => Some("{"),
                    '}' => Some("}"),
                    '(' => Some("("),
                    ')' => Some(")"),
                    '[' => Some("["),
                    ']' => Some("]"),
                    ':' => Some(":"),
                    '=' => Some("="),
                    '!' => Some("!"),
                    '|' => Some("|"),
                    '&' => Some("&"),
                    ',' => Some(","),
                    _ => None,
                };
                out.push(Token { tok: p.map_or(Tok::Other(c), Tok::Punct), line });
                i += 1;
            }
        }
    }
    out
}

/// Light checks before sending: balanced braces/parentheses, an operation present, variables used vs declared. Returns problems (empty = fine).
pub fn lint(query: &str) -> Vec<String> {
    let toks: Vec<Token> = lex(query).into_iter().filter(|t| !matches!(t.tok, Tok::Comment(_))).collect();
    let mut problems = Vec::new();
    if toks.is_empty() {
        return vec!["The query is empty".into()];
    }

    // Brackets.
    let mut stack: Vec<(&str, usize)> = Vec::new();
    for t in &toks {
        match &t.tok {
            Tok::Punct(p @ ("{" | "(" | "[")) => stack.push((p, t.line)),
            Tok::Punct(p @ ("}" | ")" | "]")) => {
                let open = match *p {
                    "}" => "{",
                    ")" => "(",
                    _ => "[",
                };
                match stack.pop() {
                    Some((o, _)) if o == open => {}
                    Some((o, l)) => {
                        problems.push(format!("Line {}: '{p}' closes the '{o}' of line {l}", t.line));
                        stack.clear();
                        break;
                    }
                    None => {
                        problems.push(format!("Line {}: '{p}' has nothing to close", t.line));
                        break;
                    }
                }
            }
            Tok::BadString(_) => problems.push(format!("Line {}: unterminated string", t.line)),
            Tok::Other(c) => problems.push(format!("Line {}: unexpected character '{c}'", t.line)),
            _ => {}
        }
    }
    for (o, l) in stack {
        problems.push(format!("Line {l}: '{o}' is never closed"));
    }
    if !problems.is_empty() {
        return problems;
    }

    // Definitions at the top level: [keyword] [name] [(variables)] [directives] { … }.
    struct Def {
        fragment: bool,
        name: Option<String>,
        declared: Vec<String>,
        used: BTreeSet<String>,
        line: usize,
    }
    let mut defs: Vec<Def> = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let line = toks[i].line;
        let mut def = Def { fragment: false, name: None, declared: Vec::new(), used: BTreeSet::new(), line };
        match &toks[i].tok {
            Tok::Name(k) if matches!(k.as_str(), "query" | "mutation" | "subscription") => {
                i += 1;
                if let Some(Token { tok: Tok::Name(n), .. }) = toks.get(i) {
                    def.name = Some(n.clone());
                    i += 1;
                }
            }
            Tok::Name(k) if k == "fragment" => {
                def.fragment = true;
                // fragment Name on Type
                i += 1;
                if let Some(Token { tok: Tok::Name(n), .. }) = toks.get(i) {
                    def.name = Some(n.clone());
                }
                while i < toks.len() && toks[i].tok != Tok::Punct("{") {
                    i += 1;
                }
            }
            Tok::Punct("{") => {}
            Tok::Name(k) => {
                problems.push(format!("Line {line}: expected query, mutation, subscription, fragment or '{{', found '{k}'"));
                return problems;
            }
            _ => {
                problems.push(format!("Line {line}: expected query, mutation, subscription, fragment or '{{'"));
                return problems;
            }
        }
        // Variable definitions and directives, up to the selection set.
        let mut depth = 0;
        while i < toks.len() && !(depth == 0 && toks[i].tok == Tok::Punct("{")) {
            match &toks[i].tok {
                Tok::Punct("(") => depth += 1,
                Tok::Punct(")") => depth -= 1,
                Tok::Var(v) if depth == 1 && toks.get(i + 1).is_some_and(|t| t.tok == Tok::Punct(":")) => {
                    if def.declared.contains(v) {
                        problems.push(format!("Line {}: ${v} is declared twice", toks[i].line));
                    }
                    def.declared.push(v.clone());
                }
                Tok::Var(v) => {
                    def.used.insert(v.clone());
                }
                _ => {}
            }
            i += 1;
        }
        if i >= toks.len() {
            problems.push(format!("Line {line}: the definition has no selection set {{ … }}"));
            break;
        }
        // The selection set.
        let mut depth = 0;
        let mut empty = true;
        while i < toks.len() {
            match &toks[i].tok {
                Tok::Punct("{") => depth += 1,
                Tok::Punct("}") => {
                    depth -= 1;
                    if depth == 0 {
                        i += 1;
                        break;
                    }
                }
                Tok::Var(v) => {
                    def.used.insert(v.clone());
                }
                Tok::Name(_) | Tok::Punct("...") => empty = false,
                _ => {}
            }
            i += 1;
        }
        if empty {
            problems.push(format!("Line {line}: empty selection set"));
        }
        defs.push(def);
    }

    let ops: Vec<&Def> = defs.iter().filter(|d| !d.fragment).collect();
    if ops.is_empty() {
        problems.push("No operation: only fragments (add query { … })".into());
    }
    if ops.len() > 1 && ops.iter().any(|d| d.name.is_none()) {
        problems.push("An operation without a name must be the only one in the document".into());
    }
    let mut names = HashSet::new();
    for d in &defs {
        if let Some(n) = &d.name {
            if !names.insert((d.fragment, n)) {
                problems.push(format!("Line {}: {} '{n}' is defined twice", d.line, if d.fragment { "fragment" } else { "operation" }));
            }
        }
    }
    // Fragments' variables come from whichever operation uses them: checked against all operations.
    let fragment_vars: BTreeSet<&String> = defs.iter().filter(|d| d.fragment).flat_map(|d| &d.used).collect();
    for op in &ops {
        let label = op.name.as_ref().map_or("The query".to_string(), |n| format!("Operation {n}"));
        for v in &op.used {
            if !op.declared.contains(v) {
                problems.push(format!("{label} uses ${v} without declaring it (add ${v}: Type in its parentheses)"));
            }
        }
        for v in &op.declared {
            if !op.used.contains(v) && !fragment_vars.contains(v) {
                problems.push(format!("{label} declares ${v} but never uses it"));
            }
        }
    }
    for v in fragment_vars {
        if !ops.iter().any(|o| o.declared.contains(v)) {
            problems.push(format!("A fragment uses ${v}, which no operation declares"));
        }
    }
    problems
}

/// Pretty-prints a query (indentation by braces), keeping strings intact.
pub fn format_query(query: &str) -> String {
    let toks = lex(query);
    let mut out = String::new();
    let mut indent: usize = 0;
    // Brace depth of selection sets, and of `(`/`[`/`{` inside arguments (kept on one line).
    let mut inline: Vec<&str> = Vec::new();
    let mut at_line_start = true;
    let mut prev: Option<&Tok> = None;
    let mut prev2: Option<&Tok> = None;
    let mut prev_line = 0;
    let mut blank_before = false;

    fn newline(out: &mut String, at_line_start: &mut bool) {
        if !*at_line_start {
            out.push('\n');
            *at_line_start = true;
        }
    }

    for t in &toks {
        let tok = &t.tok;
        let block = inline.is_empty() && indent > 0;
        if let Tok::Comment(c) = tok {
            if !at_line_start && t.line == prev_line {
                out.push_str("  ");
            } else {
                newline(&mut out, &mut at_line_start);
                out.push_str(&"  ".repeat(indent));
            }
            out.push_str(c);
            out.push('\n');
            at_line_start = true;
            prev_line = t.line;
            continue;
        }
        if matches!(tok, Tok::Punct(",")) && inline.is_empty() {
            prev_line = t.line;
            continue; // commas between fields mean nothing
        }
        // A new field / spread starts its own line inside a selection set.
        let joined = matches!(prev, Some(Tok::Punct(":" | "..."))) || matches!((prev2, prev), (Some(Tok::Punct("...")), Some(Tok::Name(n))) if n == "on");
        let starts_item = block && matches!(tok, Tok::Name(_) | Tok::Punct("...")) && !joined;
        if starts_item || matches!(tok, Tok::Punct("}")) && inline.is_empty() {
            newline(&mut out, &mut at_line_start);
        }
        if blank_before && at_line_start && indent == 0 {
            if !out.is_empty() {
                out.push('\n');
            }
            blank_before = false;
        }
        match tok {
            Tok::Punct("{") if inline.is_empty() => {
                if !at_line_start {
                    out.push(' ');
                } else {
                    out.push_str(&"  ".repeat(indent));
                }
                out.push('{');
                indent += 1;
                out.push('\n');
                at_line_start = true;
            }
            Tok::Punct("}") if inline.is_empty() => {
                indent = indent.saturating_sub(1);
                out.push_str(&"  ".repeat(indent));
                out.push('}');
                out.push('\n');
                at_line_start = true;
                if indent == 0 {
                    blank_before = true;
                }
            }
            _ => {
                if at_line_start {
                    out.push_str(&"  ".repeat(indent));
                } else if needs_space(prev, tok) {
                    out.push(' ');
                }
                at_line_start = false;
                match tok {
                    Tok::Punct(p @ ("(" | "[" | "{")) => {
                        inline.push(p);
                        out.push_str(p);
                    }
                    Tok::Punct(p @ (")" | "]" | "}")) => {
                        inline.pop();
                        out.push_str(p);
                    }
                    Tok::Punct(p) => out.push_str(p),
                    Tok::Name(s) | Tok::Value(s) | Tok::BadString(s) => out.push_str(s),
                    Tok::Var(s) => {
                        out.push('$');
                        out.push_str(s);
                    }
                    Tok::Directive(s) => {
                        out.push('@');
                        out.push_str(s);
                    }
                    Tok::Other(c) => out.push(*c),
                    Tok::Comment(_) => {}
                }
            }
        }
        prev2 = prev;
        prev = Some(tok);
        prev_line = t.line;
    }
    let trimmed = out.trim_end();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}\n")
    }
}

/// Between two tokens on one line: `user(id: $id, first: 10)`, `[ID!]`, `$a: Int = 1`.
fn needs_space(prev: Option<&Tok>, cur: &Tok) -> bool {
    let Some(prev) = prev else { return false };
    if matches!(prev, Tok::Punct("...")) {
        return matches!(cur, Tok::Directive(_)) || matches!(cur, Tok::Name(n) if n == "on");
    }
    if matches!(prev, Tok::Punct("(" | "[" | "{")) {
        return false;
    }
    !matches!(cur, Tok::Punct(")" | "]" | "}" | ":" | "," | "!" | "(" | "["))
        || matches!(cur, Tok::Punct("(" | "[")) && matches!(prev, Tok::Punct(":" | "," | "=" | "!" | "("))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hand-written introspection result, in the shape `INTROSPECTION_QUERY` returns.
    fn fixture() -> String {
        fn named(kind: &str, name: &str) -> Value {
            json!({"kind": kind, "name": name, "ofType": null})
        }
        fn non_null(t: Value) -> Value {
            json!({"kind": "NON_NULL", "name": null, "ofType": t})
        }
        fn list_of(t: Value) -> Value {
            json!({"kind": "LIST", "name": null, "ofType": t})
        }
        fn field(name: &str, ty: Value, args: Vec<Value>) -> Value {
            json!({"name": name, "description": format!("The {name}"), "args": args, "type": ty, "isDeprecated": name == "legacy", "deprecationReason": null})
        }
        fn arg(name: &str, ty: Value, default: Option<&str>) -> Value {
            json!({"name": name, "description": "", "type": ty, "defaultValue": default})
        }
        fn object(name: &str, fields: Vec<Value>) -> Value {
            json!({"kind": "OBJECT", "name": name, "description": "", "fields": fields, "inputFields": null, "interfaces": [], "enumValues": null, "possibleTypes": null})
        }
        let scalar = |n: &str| json!({"kind": "SCALAR", "name": n, "description": "", "fields": null, "inputFields": null, "interfaces": null, "enumValues": null, "possibleTypes": null});
        let types = vec![
            object(
                "Query",
                vec![
                    field("user", named("OBJECT", "User"), vec![arg("id", non_null(named("SCALAR", "ID")), None)]),
                    field("users", non_null(list_of(non_null(named("OBJECT", "User")))), vec![arg("first", named("SCALAR", "Int"), Some("10"))]),
                    field("search", list_of(named("UNION", "SearchResult")), vec![arg("text", non_null(named("SCALAR", "String")), None)]),
                    field("version", non_null(named("SCALAR", "String")), vec![]),
                ],
            ),
            object(
                "Mutation",
                vec![field(
                    "create_post",
                    named("OBJECT", "Post"),
                    vec![arg("input", non_null(named("INPUT_OBJECT", "PostInput")), None), arg("notify", named("SCALAR", "Boolean"), Some("false"))],
                )],
            ),
            object(
                "User",
                vec![
                    field("id", non_null(named("SCALAR", "ID")), vec![]),
                    field("name", named("SCALAR", "String"), vec![]),
                    field("role", named("ENUM", "Role"), vec![]),
                    field("posts", list_of(named("OBJECT", "Post")), vec![]),
                    field("avatar", named("SCALAR", "String"), vec![arg("size", non_null(named("SCALAR", "Int")), None)]),
                    field("legacy", named("SCALAR", "String"), vec![]),
                ],
            ),
            object(
                "Post",
                vec![
                    field("id", non_null(named("SCALAR", "ID")), vec![]),
                    field("title", named("SCALAR", "String"), vec![]),
                    field("author", named("OBJECT", "User"), vec![]),
                ],
            ),
            json!({"kind": "UNION", "name": "SearchResult", "description": "", "fields": null, "inputFields": null, "interfaces": null, "enumValues": null,
                   "possibleTypes": [named("OBJECT", "User"), named("OBJECT", "Post")]}),
            json!({"kind": "ENUM", "name": "Role", "description": "Who", "fields": null, "inputFields": null, "interfaces": null,
                   "enumValues": [{"name": "ADMIN", "description": "", "isDeprecated": false, "deprecationReason": null},
                                  {"name": "MEMBER", "description": "", "isDeprecated": false, "deprecationReason": null}], "possibleTypes": null}),
            json!({"kind": "INPUT_OBJECT", "name": "PostInput", "description": "", "fields": null, "interfaces": null, "enumValues": null, "possibleTypes": null,
                   "inputFields": [arg("title", non_null(named("SCALAR", "String")), None), arg("role", named("ENUM", "Role"), None),
                                   arg("tags", list_of(non_null(named("SCALAR", "String"))), None), arg("score", named("SCALAR", "Float"), Some("1.5"))]}),
            scalar("ID"),
            scalar("String"),
            scalar("Int"),
            scalar("Float"),
            scalar("Boolean"),
        ];
        json!({"data": {"__schema": {"queryType": {"name": "Query"}, "mutationType": {"name": "Mutation"}, "subscriptionType": null, "types": types, "directives": []}}})
            .to_string()
    }

    #[test]
    fn parses_the_schema() {
        let s = parse_introspection(&fixture()).unwrap();
        assert_eq!(s.query_type.as_deref(), Some("Query"));
        assert_eq!(s.mutation_type.as_deref(), Some("Mutation"));
        assert_eq!(s.subscription_type, None);
        let users = s.root_fields("query").into_iter().find(|f| f.name == "users").unwrap();
        assert_eq!(users.type_ref, "[User!]!");
        assert_eq!(users.base_type, "User");
        assert_eq!(users.args[0].type_ref, "Int");
        assert_eq!(users.args[0].default.as_deref(), Some("10"));
        assert_eq!(s.get("Role").unwrap().enum_values, ["ADMIN", "MEMBER"]);
        assert_eq!(s.get("SearchResult").unwrap().possible_types, ["User", "Post"]);
        assert_eq!(s.get("PostInput").unwrap().input_fields[2].type_ref, "[String!]");
        assert!(s.get("User").unwrap().fields.iter().any(|f| f.name == "legacy" && f.deprecated));
        assert_eq!(s.root_fields("subscription").len(), 0);
        assert_eq!(s.root_fields("mutation")[0].name, "create_post");

        // Without the `data` wrapper.
        let v: Value = serde_json::from_str(&fixture()).unwrap();
        let bare = parse_introspection(&v["data"].to_string()).unwrap();
        assert_eq!(bare.types.len(), s.types.len());
    }

    #[test]
    fn introspection_errors() {
        let e = parse_introspection(r#"{"errors": [{"message": "Introspection is disabled"}], "data": null}"#).unwrap_err();
        assert!(e.contains("Introspection is disabled"), "{e}");
        assert!(parse_introspection("<html>").unwrap_err().contains("isn't JSON"));
        assert!(parse_introspection(r#"{"data": {}}"#).unwrap_err().contains("__schema"));
        assert!(INTROSPECTION_QUERY.contains("possibleTypes") && lint(INTROSPECTION_QUERY).is_empty(), "{:?}", lint(INTROSPECTION_QUERY));
        assert_eq!(INTROSPECTION_QUERY.matches("ofType {").count(), 7);
    }

    #[test]
    fn operation_with_required_argument() {
        let s = parse_introspection(&fixture()).unwrap();
        let (q, vars) = operation_for(&s, "query", "user", 1).unwrap();
        assert_eq!(q, "query User($id: ID!) {\n  user(id: $id) {\n    id\n    name\n    role\n    posts {\n      id\n      title\n    }\n    legacy\n  }\n}\n");
        assert_eq!(serde_json::from_str::<Value>(&vars).unwrap(), json!({"id": ""}));
        assert!(lint(&q).is_empty(), "{:?}", lint(&q));
        // depth 0: scalars only, no `posts`
        let (q0, _) = operation_for(&s, "query", "user", 0).unwrap();
        assert!(!q0.contains("posts") && q0.contains("    name\n"), "{q0}");
    }

    #[test]
    fn operation_with_defaults_unions_and_inputs() {
        let s = parse_introspection(&fixture()).unwrap();
        let (q, vars) = operation_for(&s, "query", "users", 0).unwrap();
        assert!(q.starts_with("query Users($first: Int = 10) {\n  users(first: $first) {"), "{q}");
        assert_eq!(serde_json::from_str::<Value>(&vars).unwrap(), json!({"first": 10}));

        let (q, _) = operation_for(&s, "query", "search", 1).unwrap();
        assert!(q.contains("    __typename\n    ... on User {\n      id\n"), "{q}");
        assert!(q.contains("    ... on Post {\n      id\n      title\n    }\n"), "{q}");
        assert!(lint(&q).is_empty(), "{:?}", lint(&q));

        let (q, _) = operation_for(&s, "query", "version", 2).unwrap();
        assert_eq!(q, "query Version {\n  version\n}\n");

        let (q, vars) = operation_for(&s, "mutation", "create_post", 1).unwrap();
        assert!(q.starts_with("mutation CreatePost($input: PostInput!, $notify: Boolean = false) {\n  create_post(input: $input, notify: $notify) {"), "{q}");
        assert!(q.contains("author {\n      id\n"), "{q}");
        assert_eq!(serde_json::from_str::<Value>(&vars).unwrap(), json!({"input": {"title": "", "role": "ADMIN", "tags": [], "score": 1.5}, "notify": false}));
        assert!(operation_for(&s, "query", "nope", 1).is_none());
        assert!(operation_for(&s, "subscription", "user", 1).is_none());
    }

    #[test]
    fn lint_cases() {
        assert!(lint("{ me { id } }").is_empty());
        assert!(lint("query Q($id: ID!) { user(id: $id) { ...F } }\nfragment F on User { name }").is_empty());
        assert_eq!(lint("   "), ["The query is empty"]);
        assert_eq!(lint("query { user { id }"), ["Line 1: '{' is never closed"]);
        assert_eq!(lint("query { user(id: 1 { id } }"), ["Line 1: '}' closes the '(' of line 1"]);
        assert_eq!(lint("{ a } }"), ["Line 1: '}' has nothing to close"]);
        assert_eq!(lint("query Q { user(id: $id) { id } }"), ["Operation Q uses $id without declaring it (add $id: Type in its parentheses)"]);
        assert_eq!(lint("query Q($id: ID, $x: Int) { user(id: $id) { id } }"), ["Operation Q declares $x but never uses it"]);
        assert_eq!(lint("fragment F on User { id }"), ["No operation: only fragments (add query { … })"]);
        assert!(lint("querry { a }")[0].contains("found 'querry'"));
        assert!(lint("{ a } { b }")[0].contains("without a name"));
        assert!(lint("query { }")[0].contains("empty selection set"));
        assert!(lint("query { a(s: \"x) }")[0].contains("unterminated string"));
        // Braces and `$` inside strings and comments don't count.
        assert!(lint("query { a(s: \"{ $nope (\") # } $x\n }").is_empty(), "{:?}", lint("query { a(s: \"{ $nope (\") # }\n }"));
        assert!(lint("query Q($v: Int) { ...F }\nfragment F on Query { a(v: $v) }").is_empty());
        assert_eq!(lint("query Q { ...F }\nfragment F on Query { a(v: $v) }"), ["A fragment uses $v, which no operation declares"]);
    }

    #[test]
    fn formats_queries() {
        let q = r#"query Q($id: ID!, $n: [Int!] = [1, 2]) { user(id: $id, filter: {name: "a { b } # c", n: $n}) { id, name ...F ... on Admin { level } alias: posts(first: 2) @include(if: true) { title } } }
fragment F on User { email # trailing
}"#;
        let expected = r#"query Q($id: ID!, $n: [Int!] = [1, 2]) {
  user(id: $id, filter: {name: "a { b } # c", n: $n}) {
    id
    name
    ...F
    ... on Admin {
      level
    }
    alias: posts(first: 2) @include(if: true) {
      title
    }
  }
}

fragment F on User {
  email  # trailing
}
"#;
        assert_eq!(format_query(q), expected);
        assert_eq!(format_query(expected), expected, "idempotent");
        assert_eq!(format_query("{a{b}}"), "{\n  a {\n    b\n  }\n}\n");
        let block = "query {\n  a(text: \"\"\"line 1\n  { line 2 }\"\"\")\n}\n";
        assert_eq!(format_query(block), block);
        // Broken input doesn't panic.
        let _ = format_query("}}} { ( [ \"");
        assert_eq!(format_query(""), "");
    }

    #[test]
    fn request_bodies() {
        let b: Value = serde_json::from_str(&request_body("{ a }", r#"{"id": 1}"#, "Q").unwrap()).unwrap();
        assert_eq!(b, json!({"query": "{ a }", "variables": {"id": 1}, "operationName": "Q"}));
        let b: Value = serde_json::from_str(&request_body("{ a }", "  ", "").unwrap()).unwrap();
        assert_eq!(b, json!({"query": "{ a }"}));
        let e = request_body("{ a }", "{\"id\": }", "").unwrap_err();
        assert!(e.contains("aren't valid JSON") && e.contains("line 1"), "{e}");
        assert!(request_body("{ a }", "[1]", "").unwrap_err().contains("a list"));
    }
}
