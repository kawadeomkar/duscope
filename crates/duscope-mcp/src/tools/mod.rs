//! The tool registry: what `tools/list` lists and `tools/call` runs.
//!
//! A tool is a verb seen from MCP. It names itself, describes itself, carries
//! its hand-written input schema, and turns a call's arguments into its
//! verb's command line, every path and saved scan going through [`Roots`]
//! first. The binary runs that command line in its own process, as
//! `duscope <verb> --json` would run ([`Runner`]), and the result carries
//! what the verb wrote, byte for byte, as `structuredContent`, beside a
//! one-line text block. A verb that fails, and a call refused before its
//! verb runs, answer with `isError: true`, the error envelope, and the
//! error's message and hint as the text, which tells the model what to do
//! next.
//!
//! `tools/list` is the same in every answer: the tools sorted by name, each
//! with its annotations (`openWorldHint` always false) and
//! `_meta["anthropic/maxResultSizeChars"]` equal to `--max-chars`, past
//! which the verb puts its envelope in a file and returns the stub. A tool
//! that deletes is listed only under `--allow-delete`.
//!
//! The input schemas are a subset of JSON Schema that `validate` checks in
//! full: an object of named properties, each a string, integer, number,
//! boolean or array of those, with `enum`, bounds, lengths and `pattern`.
//! Registering a schema that uses any other keyword fails, so no part of a
//! schema goes unchecked.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

use duscope_core::query::Regex;
use serde::Serialize;
use serde_json::value::RawValue;
use serde_json::{Map, Value, json};

use crate::jsonrpc::RpcError;
use crate::roots::{Refusal, Roots, Visibility};

mod by_type;
mod scan;
mod scans;
mod stale;
mod summary;
mod top;

/// The tools duscope offers.
pub fn shipped() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(scan::Scan),
        Box::new(scans::Scans),
        Box::new(summary::Summary),
        Box::new(top::Top),
        Box::new(by_type::ByType),
        Box::new(stale::Stale),
    ]
}

/// The longest tool name: Gemini CLI cuts names at 63 characters, and
/// Claude Code drops `mcp__<server>__<tool>` past 128.
pub const MAX_NAME_CHARS: usize = 63;

/// What a tool may do, as its annotations tell a client. `openWorldHint` is
/// always false: duscope reads the local file system, nothing beyond it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Annotations {
    /// It changes nothing.
    pub read_only: bool,
    /// It may delete or overwrite something.
    pub destructive: bool,
    /// Calling it twice with the same arguments does what calling it once
    /// does.
    pub idempotent: bool,
}

/// A verb's command line: the verb and its arguments, without `duscope`,
/// `--json` or the server's own flags, which the runner adds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// The verb.
    pub verb: &'static str,
    /// Its flags and paths.
    pub args: Vec<OsString>,
}

/// What a verb run for a tool wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The JSON envelope, as `--json` writes it, without its newline: the
    /// verb's result, the spill stub, or the error envelope.
    pub json: String,
    /// Whether the verb succeeded (exit 0).
    pub ok: bool,
}

/// What a tool reads besides its arguments.
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    /// The roots every path and saved scan goes through.
    pub roots: &'a Roots,
    /// Where the saved scans are.
    pub index_dir: &'a Path,
}

/// A tool: a verb as an MCP client calls it.
pub trait Tool: Send + Sync {
    /// The tool's name, `duscope_<verb>`.
    fn name(&self) -> &'static str;

    /// What the tool answers, for the model choosing a tool.
    fn description(&self) -> &'static str;

    /// The input schema, as JSON text.
    fn input_schema(&self) -> &'static str;

    /// The output schema, as JSON text, when the tool declares one.
    fn output_schema(&self) -> Option<&'static str> {
        None
    }

    /// What the tool may do.
    fn annotations(&self) -> Annotations;

    /// Whether a client must ask its user before every call, whatever its
    /// approval mode (`_meta["anthropic/requiresUserInteraction"]`).
    fn requires_user_interaction(&self) -> bool {
        false
    }

    /// The command line for `arguments`, which the input schema accepted.
    fn invocation(
        &self,
        arguments: &Map<String, Value>,
        context: &Context<'_>,
    ) -> Result<Invocation, Refusal>;

    /// The one-line text block of a result the verb returned successfully.
    fn summary(&self, envelope: &Value) -> String;
}

/// Runs a tool's verb: the binary, in its own process.
pub trait Runner: Send + Sync {
    /// Runs `invocation` as `duscope <verb> <args> --json` would, with the
    /// server's index directory and `--max-chars`, and returns what it wrote.
    fn run(&self, invocation: &Invocation) -> Outcome;

    /// The error envelope of a call refused before its verb ran, as the verb
    /// would have written it.
    fn refuse(&self, refusal: &Refusal) -> Outcome;
}

/// A tool that cannot be registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryError {
    /// The tool.
    pub tool: String,
    /// What is wrong with it.
    pub message: String,
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "tool {}: {}", self.tool, self.message)
    }
}

impl std::error::Error for RegistryError {}

/// A registered tool, its schemas read once.
struct Entry {
    tool: Box<dyn Tool>,
    input: Box<RawValue>,
    schema: Schema,
    output: Option<Box<RawValue>>,
}

/// The tools a server offers.
pub struct Registry {
    tools: BTreeMap<&'static str, Entry>,
    max_chars: usize,
    allow_delete: bool,
}

impl fmt::Debug for Registry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Registry")
            .field("tools", &self.tools.keys().collect::<Vec<_>>())
            .field("max_chars", &self.max_chars)
            .field("allow_delete", &self.allow_delete)
            .finish()
    }
}

impl Registry {
    /// An empty registry whose tools declare `max_chars`, listing the tools
    /// that delete only when `allow_delete`.
    pub fn new(max_chars: usize, allow_delete: bool) -> Registry {
        Registry {
            tools: BTreeMap::new(),
            max_chars,
            allow_delete,
        }
    }

    /// Adds `tool`, once its name and schemas pass. A destructive tool is
    /// left out without `--allow-delete`, and is not an error.
    pub fn register(&mut self, tool: Box<dyn Tool>) -> Result<(), RegistryError> {
        let name = tool.name();
        let failed = |message: String| RegistryError {
            tool: name.to_owned(),
            message,
        };
        let valid_name = !name.is_empty()
            && name.len() <= MAX_NAME_CHARS
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if !valid_name {
            return Err(failed(format!(
                "a name is 1 to {MAX_NAME_CHARS} letters, digits, `_` or `-`"
            )));
        }
        if self.tools.contains_key(name) {
            return Err(failed("registered twice".to_owned()));
        }
        if tool.annotations().destructive && !self.allow_delete {
            return Ok(());
        }
        // A schema goes out as written, its keys in the author's order, on
        // one line.
        let read = |text: &str, which: &str| -> Result<(Value, Box<RawValue>), RegistryError> {
            let parsed: Value = serde_json::from_str(text)
                .map_err(|e| failed(format!("the {which} schema is not JSON: {e}")))?;
            if !parsed.is_object() {
                return Err(failed(format!("the {which} schema is not an object")));
            }
            let raw = RawValue::from_string(minify(text))
                .map_err(|e| failed(format!("the {which} schema: {e}")))?;
            Ok((parsed, raw))
        };
        let (parsed, input) = read(tool.input_schema(), "input")?;
        let schema =
            Schema::read(&parsed).map_err(|message| failed(format!("input schema: {message}")))?;
        let output = tool
            .output_schema()
            .map(|text| read(text, "output").map(|(_, raw)| raw))
            .transpose()?;
        self.tools.insert(
            name,
            Entry {
                tool,
                input,
                schema,
                output,
            },
        );
        Ok(())
    }

    /// The registered tools' names, sorted.
    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.tools.keys().copied()
    }

    /// `tools/list`'s `tools`, sorted by name.
    pub fn list(&self) -> Vec<ListedTool<'_>> {
        self.tools
            .values()
            .map(|entry| {
                let annotations = entry.tool.annotations();
                let mut meta = Map::new();
                meta.insert(
                    "anthropic/maxResultSizeChars".to_owned(),
                    json!(self.max_chars),
                );
                if entry.tool.requires_user_interaction() {
                    meta.insert("anthropic/requiresUserInteraction".to_owned(), json!(true));
                }
                ListedTool {
                    name: entry.tool.name(),
                    description: entry.tool.description(),
                    input_schema: &entry.input,
                    output_schema: entry.output.as_deref(),
                    annotations: ListedAnnotations {
                        read_only_hint: annotations.read_only,
                        destructive_hint: annotations.destructive,
                        idempotent_hint: annotations.idempotent,
                        open_world_hint: false,
                    },
                    meta,
                }
            })
            .collect()
    }

    /// Answers `tools/call`'s `params`: an unknown tool or malformed params
    /// are a protocol error; arguments the schema refuses, a path or scan the
    /// roots refuse, and a verb that fails are a result with `isError`.
    pub fn call(
        &self,
        params: Option<&Map<String, Value>>,
        context: &Context<'_>,
        runner: &dyn Runner,
    ) -> Result<CallResult, RpcError> {
        let params = params.ok_or_else(|| RpcError::invalid_params("tools/call needs params"))?;
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::invalid_params("tools/call's name must be a string"))?;
        let entry = self.tools.get(name).ok_or_else(|| {
            RpcError::invalid_params(format!(
                "unknown tool {}: tools/list lists the tools",
                name.escape_debug()
            ))
        })?;
        let empty = Map::new();
        let arguments = match params.get("arguments") {
            None | Some(Value::Null) => &empty,
            Some(Value::Object(arguments)) => arguments,
            Some(_) => {
                return Err(RpcError::invalid_params(
                    "tools/call's arguments must be an object",
                ));
            }
        };
        let invocation = entry
            .schema
            .check(name, arguments)
            .and_then(|()| entry.tool.invocation(arguments, context));
        let outcome = match &invocation {
            Ok(invocation) => runner.run(invocation),
            Err(refusal) => runner.refuse(refusal),
        };
        CallResult::new(entry.tool.as_ref(), outcome)
    }
}

/// One tool as `tools/list` lists it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListedTool<'a> {
    name: &'static str,
    description: &'static str,
    input_schema: &'a RawValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_schema: Option<&'a RawValue>,
    annotations: ListedAnnotations,
    #[serde(rename = "_meta")]
    meta: Map<String, Value>,
}

/// The protocol's four hints, each a boolean.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)]
struct ListedAnnotations {
    read_only_hint: bool,
    destructive_hint: bool,
    idempotent_hint: bool,
    open_world_hint: bool,
}

/// A text content block.
#[derive(Debug, Serialize)]
struct Text {
    #[serde(rename = "type")]
    kind: &'static str,
    text: String,
}

/// `tools/call`'s result: the text block, the envelope, and whether it is
/// an error.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallResult {
    content: [Text; 1],
    structured_content: Box<RawValue>,
    is_error: bool,
}

impl CallResult {
    /// The result of `tool`'s verb, which wrote `outcome`.
    fn new(tool: &dyn Tool, outcome: Outcome) -> Result<CallResult, RpcError> {
        let parsed: Value = serde_json::from_str(&outcome.json).map_err(|e| {
            RpcError::internal(format!("{} wrote no JSON envelope: {e}", tool.name()))
        })?;
        let text = match parsed.get("spilled").filter(|_| outcome.ok) {
            Some(spill) => spilled_text(spill),
            None if outcome.ok => tool.summary(&parsed),
            None => error_text(&parsed),
        };
        // Compact JSON holds no newline, which would end the message early;
        // anything else is written again.
        let raw = if outcome.json.contains('\n') {
            serde_json::value::to_raw_value(&parsed)
        } else {
            RawValue::from_string(outcome.json)
        }
        .map_err(|e| RpcError::internal(format!("{}: {e}", tool.name())))?;
        Ok(CallResult {
            content: [Text {
                kind: "text",
                text: one_line(&text),
            }],
            structured_content: raw,
            is_error: !outcome.ok,
        })
    }

    /// Whether the result is an error.
    pub fn is_error(&self) -> bool {
        self.is_error
    }

    /// The envelope, as the verb wrote it.
    pub fn structured_content(&self) -> &str {
        self.structured_content.get()
    }
}

/// An error envelope's message and hint, as one line of two sentences.
fn error_text(envelope: &Value) -> String {
    let error = &envelope["error"];
    let message = error["message"].as_str().unwrap_or("the verb failed");
    let stop = if message.ends_with(['.', '!', '?']) {
        ""
    } else {
        "."
    };
    match error["hint"].as_str() {
        Some(hint) => format!("{message}{stop} {hint}"),
        None => format!("{message}{stop}"),
    }
}

/// The text of a result the verb put in a file, past `--max-chars`: where
/// the file is, and what to do instead.
fn spilled_text(spill: &Value) -> String {
    format!(
        "The result passed the server's --max-chars: its {} rows are in {} ({} bytes), and \
         the preview holds the first. Ask for fewer rows or fields, or read the file",
        spill["rows"],
        spill["path"].as_str().unwrap_or("a file"),
        spill["bytes"]
    )
}

/// `text` on one line: line breaks become spaces.
fn one_line(text: &str) -> String {
    text.split(['\n', '\r'])
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// JSON text without the whitespace between its tokens, strings untouched.
fn minify(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut escaped = false;
    for c in text.chars() {
        if in_string {
            out.push(c);
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => in_string = false,
                _ => {}
            }
        } else if c == '"' {
            in_string = true;
            out.push(c);
        } else if !c.is_whitespace() {
            out.push(c);
        }
    }
    out
}

/// The input schema, read into the checks `validate` makes.
#[derive(Debug, Clone)]
struct Schema {
    properties: BTreeMap<String, Property>,
    required: Vec<String>,
}

/// One property's checks.
#[derive(Debug, Clone)]
struct Property {
    kind: Kind,
    items: Option<Box<Property>>,
    choices: Option<Vec<Value>>,
    minimum: Option<f64>,
    maximum: Option<f64>,
    min_length: Option<u64>,
    max_length: Option<u64>,
    pattern: Option<Regex>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    String,
    Integer,
    Number,
    Boolean,
    Array,
}

/// Keywords that describe and check nothing.
const ANNOTATION_KEYWORDS: [&str; 6] = [
    "$schema",
    "$id",
    "title",
    "description",
    "default",
    "examples",
];

impl Schema {
    /// The checks `schema` asks for, or what in it `validate` cannot
    /// check.
    fn read(schema: &Value) -> Result<Schema, String> {
        let object = schema.as_object().ok_or("not an object")?;
        for key in object.keys() {
            let known = ["type", "properties", "required", "additionalProperties"];
            if !known.contains(&key.as_str()) && !ANNOTATION_KEYWORDS.contains(&key.as_str()) {
                return Err(format!("the keyword {key} is not one the server checks"));
            }
        }
        if object.get("type") != Some(&json!("object")) {
            return Err("its type must be object".to_owned());
        }
        if object.get("additionalProperties") != Some(&json!(false)) {
            return Err("it must set additionalProperties to false".to_owned());
        }
        let mut properties = BTreeMap::new();
        if let Some(listed) = object.get("properties") {
            let listed = listed.as_object().ok_or("properties is not an object")?;
            for (name, property) in listed {
                let read = Property::read(property).map_err(|e| format!("{name}: {e}"))?;
                properties.insert(name.clone(), read);
            }
        }
        let required = match object.get("required") {
            None => Vec::new(),
            Some(names) => names
                .as_array()
                .ok_or("required is not an array")?
                .iter()
                .map(|name| {
                    let name = name.as_str().ok_or("required lists a non-string")?;
                    if properties.contains_key(name) {
                        Ok(name.to_owned())
                    } else {
                        Err(format!("required names {name}, which is not a property"))
                    }
                })
                .collect::<Result<_, String>>()?,
        };
        Ok(Schema {
            properties,
            required,
        })
    }

    /// Checks `arguments`: a refusal naming the first argument wrong.
    fn check(&self, tool: &str, arguments: &Map<String, Value>) -> Result<(), Refusal> {
        validate(self, arguments).map_err(|message| Refusal {
            code: "usage",
            message,
            hint: format!("Call {tool} with the arguments its input schema in tools/list names"),
        })
    }
}

impl Property {
    fn read(schema: &Value) -> Result<Property, String> {
        let object = schema.as_object().ok_or("not an object")?;
        let known = [
            "type",
            "items",
            "enum",
            "minimum",
            "maximum",
            "minLength",
            "maxLength",
            "pattern",
        ];
        for key in object.keys() {
            if !known.contains(&key.as_str()) && !ANNOTATION_KEYWORDS.contains(&key.as_str()) {
                return Err(format!("the keyword {key} is not one the server checks"));
            }
        }
        let kind = match object.get("type").and_then(Value::as_str) {
            Some("string") => Kind::String,
            Some("integer") => Kind::Integer,
            Some("number") => Kind::Number,
            Some("boolean") => Kind::Boolean,
            Some("array") => Kind::Array,
            _ => return Err("type must be string, integer, number, boolean or array".to_owned()),
        };
        let items = match (kind, object.get("items")) {
            (Kind::Array, Some(items)) => {
                let items = Property::read(items).map_err(|e| format!("items: {e}"))?;
                if items.kind == Kind::Array {
                    return Err("items may not be arrays".to_owned());
                }
                Some(Box::new(items))
            }
            (Kind::Array, None) => return Err("an array needs items".to_owned()),
            (_, Some(_)) => return Err("items is for arrays".to_owned()),
            (_, None) => None,
        };
        let number = |key: &str| -> Result<Option<f64>, String> {
            object
                .get(key)
                .map(|v| v.as_f64().ok_or(format!("{key} is not a number")))
                .transpose()
        };
        let count = |key: &str| -> Result<Option<u64>, String> {
            object
                .get(key)
                .map(|v| v.as_u64().ok_or(format!("{key} is not a count")))
                .transpose()
        };
        let pattern = object
            .get("pattern")
            .map(|p| {
                let p = p.as_str().ok_or("pattern is not a string")?;
                Regex::new(p).map_err(|e| format!("pattern: {e}"))
            })
            .transpose()?;
        let choices = object
            .get("enum")
            .map(|v| v.as_array().cloned().ok_or("enum is not an array"))
            .transpose()?;
        Ok(Property {
            kind,
            items,
            choices,
            minimum: number("minimum")?,
            maximum: number("maximum")?,
            min_length: count("minLength")?,
            max_length: count("maxLength")?,
            pattern,
        })
    }

    /// What `value` should be, for a message.
    fn wanted(&self) -> String {
        if let Some(choices) = &self.choices {
            let listed: Vec<String> = choices.iter().map(Value::to_string).collect();
            return format!("one of {}", listed.join(", "));
        }
        let kind = match self.kind {
            Kind::String => "a string",
            Kind::Integer => "an integer",
            Kind::Number => "a number",
            Kind::Boolean => "true or false",
            Kind::Array => "an array",
        };
        match (self.minimum, self.maximum) {
            (Some(lo), Some(hi)) => format!("{kind} from {lo} to {hi}"),
            (Some(lo), None) => format!("{kind} of at least {lo}"),
            (None, Some(hi)) => format!("{kind} of at most {hi}"),
            (None, None) => kind.to_owned(),
        }
    }

    /// Every check of the property, for a message: its kind and bounds,
    /// then its length and pattern; an array's items described in turn.
    fn described(&self) -> String {
        let what = match &self.items {
            Some(items) => format!("an array of ({})", items.described()),
            None => self.wanted(),
        };
        let length = match (self.min_length, self.max_length) {
            (Some(lo), Some(hi)) => format!(", {lo} to {hi} long"),
            (Some(lo), None) => format!(", at least {lo} long"),
            (None, Some(hi)) => format!(", at most {hi} long"),
            (None, None) => String::new(),
        };
        let pattern = self
            .pattern
            .as_ref()
            .map_or_else(String::new, |p| format!(", matching {}", p.as_str()));
        format!("{what}{length}{pattern}")
    }

    /// Whether `value` passes.
    fn accepts(&self, value: &Value) -> bool {
        let typed = match self.kind {
            Kind::String => value.is_string(),
            Kind::Integer => value.is_i64() || value.is_u64(),
            Kind::Number => value.is_number(),
            Kind::Boolean => value.is_boolean(),
            Kind::Array => value.is_array(),
        };
        if !typed {
            return false;
        }
        if self.choices.as_ref().is_some_and(|c| !c.contains(value)) {
            return false;
        }
        if let Some(x) = value.as_f64() {
            if self.minimum.is_some_and(|lo| x < lo) || self.maximum.is_some_and(|hi| x > hi) {
                return false;
            }
        }
        let length = match value {
            Value::String(s) => Some(s.chars().count() as u64),
            Value::Array(items) => Some(items.len() as u64),
            _ => None,
        };
        if let Some(n) = length {
            if self.min_length.is_some_and(|lo| n < lo) || self.max_length.is_some_and(|hi| n > hi)
            {
                return false;
            }
        }
        if let (Some(pattern), Some(s)) = (&self.pattern, value.as_str()) {
            if !pattern.is_match(s.as_bytes()) {
                return false;
            }
        }
        match (value, &self.items) {
            (Value::Array(values), Some(items)) => values.iter().all(|v| items.accepts(v)),
            _ => true,
        }
    }
}

/// Checks `arguments` against `schema`: every required argument present,
/// none the schema does not name, each of its type and within its bounds.
fn validate(schema: &Schema, arguments: &Map<String, Value>) -> Result<(), String> {
    if let Some(unknown) = arguments
        .keys()
        .find(|k| !schema.properties.contains_key(*k))
    {
        let names: Vec<&str> = schema.properties.keys().map(String::as_str).collect();
        return Err(if names.is_empty() {
            format!("unknown argument {unknown}: the tool takes no arguments")
        } else {
            format!(
                "unknown argument {unknown}: the tool takes {}",
                names.join(", ")
            )
        });
    }
    if let Some(missing) = schema
        .required
        .iter()
        .find(|name| !arguments.contains_key(*name))
    {
        return Err(format!("the argument {missing} is required"));
    }
    for (name, value) in arguments {
        let property = &schema.properties[name];
        if !property.accepts(value) {
            return Err(format!(
                "the argument {name} must be {}",
                property.described()
            ));
        }
    }
    Ok(())
}

/// The saved scan a call names, as the roots let the client see it: what
/// the tool passes as `--scan`, and where the call's paths are read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanArgument {
    /// `--scan`'s value: `latest`, or the scan's full id.
    pub selector: String,
    /// The scan's root, which a relative path is read from; `None` for
    /// `latest`, which names no one scan.
    pub root: Option<PathBuf>,
    /// The path the call reads when it names none: the scan's root when a
    /// root holds it, else the one root under it (as `--under` reads a
    /// subtree); `None` for `latest`, or when several roots lie under it.
    pub default_path: Option<PathBuf>,
}

/// The saved scan `arguments[name]` names, when it names one: `latest`, or
/// a scan the roots show (one they hide is `scan_not_found`), never an
/// index file.
pub fn scan_argument(
    arguments: &Map<String, Value>,
    name: &str,
    context: &Context<'_>,
) -> Result<Option<ScanArgument>, Refusal> {
    let Some(selector) = arguments.get(name).and_then(Value::as_str) else {
        return Ok(None);
    };
    let selector = context.roots.scan_selector(selector)?;
    if selector == "latest" {
        return Ok(Some(ScanArgument {
            selector: selector.to_owned(),
            root: None,
            default_path: None,
        }));
    }
    let saved = context.roots.saved_scan(context.index_dir, selector)?;
    let default_path = match &saved.visibility {
        Visibility::Above(under) => match under.as_slice() {
            [one] => Some(one.clone()),
            _ => None,
        },
        _ => Some(saved.root.clone()),
    };
    Ok(Some(ScanArgument {
        selector: saved.id,
        root: Some(saved.root),
        default_path,
    }))
}

/// The path a call reads, bound to the roots: `arguments[name]` as given,
/// absolute or relative to the scan's root; else the scan's default path;
/// else the roots' one root. With several roots and nothing to go by, the
/// call is refused.
pub fn path_argument(
    arguments: &Map<String, Value>,
    name: &str,
    scan: Option<&ScanArgument>,
    roots: &Roots,
) -> Result<PathBuf, Refusal> {
    if let Some(path) = arguments.get(name).and_then(Value::as_str) {
        let base = scan.and_then(|scan| scan.root.as_deref());
        return roots.bound_path(Path::new(path), base);
    }
    scan.and_then(|scan| scan.default_path.clone())
        .or_else(|| roots.default_path().map(Path::to_owned))
        .ok_or_else(|| Refusal {
            code: "usage",
            message: format!("the argument {name} is required when the server has several roots"),
            hint: format!("Call the tool again with {name} set to a path under one of the roots"),
        })
}

/// What a read-only tool never changes: it reads saved scans alone.
const READ_ONLY: Annotations = Annotations {
    read_only: true,
    destructive: false,
    idempotent: true,
};

/// A verb's arguments, built from a call's: each parameter the call sets
/// becomes its flag, a value joined to it by `=`, so that one starting with
/// `-` is never read as a flag.
struct CommandLine<'a> {
    arguments: &'a Map<String, Value>,
    args: Vec<OsString>,
}

impl<'a> CommandLine<'a> {
    fn new(arguments: &'a Map<String, Value>) -> CommandLine<'a> {
        CommandLine {
            arguments,
            args: Vec::new(),
        }
    }

    /// `arg`, whatever the call's arguments.
    fn push(&mut self, arg: impl Into<OsString>) -> &mut Self {
        self.args.push(arg.into());
        self
    }

    /// `flag`, when the boolean `name` is true.
    fn switch(&mut self, name: &str, flag: &str) -> &mut Self {
        if self.arguments.get(name).and_then(Value::as_bool) == Some(true) {
            self.push(flag);
        }
        self
    }

    /// `flag=VALUE`, for the string or integer `name`.
    fn value(&mut self, name: &str, flag: &str) -> &mut Self {
        let value = match self.arguments.get(name) {
            Some(Value::String(text)) => text.clone(),
            Some(Value::Number(number)) => number.to_string(),
            _ => return self,
        };
        self.push(format!("{flag}={value}"))
    }

    /// `flag=VALUE` for each string in the array `name`.
    fn each(&mut self, name: &str, flag: &str) -> &mut Self {
        for value in self.strings(name) {
            self.args.push(format!("{flag}={value}").into());
        }
        self
    }

    /// `flag=A,B,…`, for the array of strings `name` when it holds any.
    fn joined(&mut self, name: &str, flag: &str) -> &mut Self {
        let values = self.strings(name);
        if !values.is_empty() {
            self.push(format!("{flag}={}", values.join(",")));
        }
        self
    }

    /// The strings in the array `name`.
    fn strings(&self, name: &str) -> Vec<String> {
        self.arguments
            .get(name)
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The shared parameters of the list tools: `size`, `fields`, `limit`
    /// and, for a verb that pages, `cursor`.
    fn list(&mut self) -> &mut Self {
        self.value("size", "--size")
            .joined("fields", "--fields")
            .value("limit", "--limit")
            .value("cursor", "--cursor")
    }

    fn invocation(self, verb: &'static str) -> Invocation {
        Invocation {
            verb,
            args: self.args,
        }
    }
}

/// The command line of a read-only tool: `--no-scan`, so the verb never
/// scans or saves; `--scan` and the saved scan the call names; the flags
/// `flags` adds from the call's arguments; and last the path, bound to the
/// roots.
fn read_only(
    verb: &'static str,
    arguments: &Map<String, Value>,
    context: &Context<'_>,
    flags: impl FnOnce(&mut CommandLine<'_>),
) -> Result<Invocation, Refusal> {
    let scan = scan_argument(arguments, "scan_id", context)?;
    let path = path_argument(arguments, "path", scan.as_ref(), context.roots)?;
    let mut line = CommandLine::new(arguments);
    line.push("--no-scan");
    if let Some(scan) = scan {
        line.push("--scan").push(scan.selector);
    }
    flags(&mut line);
    line.push(path);
    Ok(line.invocation(verb))
}

/// The text of a list verb's envelope: how many rows of how many, under
/// which path, and how to get the next page when there is one.
fn rows_text(envelope: &Value) -> String {
    let path = envelope["query"]["path"].as_str().unwrap_or("the path");
    let rows = format!(
        "{} of {} rows under {path}",
        envelope["returned_count"], envelope["total_count"]
    );
    if envelope["next_cursor"].is_string() {
        format!("{rows}; call again with cursor set to next_cursor for the next page")
    } else {
        rows
    }
}

#[cfg(test)]
mod scan_group_tests;
#[cfg(test)]
mod tests;
