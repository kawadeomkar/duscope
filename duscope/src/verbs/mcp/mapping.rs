//! P3-MCP-04: `schemas/mcp/MAPPING.md` against the clap definitions. Every
//! flag of each shipped tool's verb has a row, every row a flag, every
//! input-schema property a row and every row's parameter a property; a
//! switch is a boolean, a repeatable flag an array, and an `enum` lists
//! values the flag takes.

use std::collections::BTreeMap;
use std::path::Path;

use clap::{Arg, ArgAction, CommandFactory};
use duscope_mcp::roots::Roots;
use duscope_mcp::tools::{Context, shipped};
use duscope_testkit::TestEnv;
use serde_json::{Map, Value};

use crate::cli::Cli;

/// A table row: the flag, the parameter that sets it (`None` for `—`), and
/// why.
#[derive(Debug, Clone)]
struct Row {
    flag: String,
    parameter: Option<String>,
    why: String,
}

/// MAPPING.md's tables: the global flags', and each tool's with its verb.
#[derive(Debug, Default)]
struct Mapping {
    global: Vec<Row>,
    tools: BTreeMap<String, (String, Vec<Row>)>,
}

/// The tables of `text`.
fn read(text: &str) -> Mapping {
    enum Section {
        Global,
        Tool(String),
        Other,
    }
    let mut mapping = Mapping::default();
    let mut section = Section::Other;
    for line in text.lines() {
        if line == "## Global flags" {
            section = Section::Global;
        } else if let Some(heading) = line.strip_prefix("### `") {
            let (tool, rest) = heading
                .split_once("` (`")
                .unwrap_or_else(|| panic!("a tool's heading is ### `tool` (`verb`): {line}"));
            let verb = rest
                .strip_suffix("`)")
                .unwrap_or_else(|| panic!("a tool's heading is ### `tool` (`verb`): {line}"));
            mapping
                .tools
                .insert(tool.to_owned(), (verb.to_owned(), Vec::new()));
            section = Section::Tool(tool.to_owned());
        } else if line.starts_with("## ") {
            section = Section::Other;
        } else if line.starts_with("| `") {
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            assert_eq!(cells.len(), 5, "a row has three cells: {line}");
            let ticked = |cell: &str| -> String {
                cell.strip_prefix('`')
                    .and_then(|c| c.strip_suffix('`'))
                    .unwrap_or_else(|| panic!("{cell} is not in backticks: {line}"))
                    .to_owned()
            };
            let row = Row {
                flag: ticked(cells[1]),
                parameter: (cells[2] != "—").then(|| ticked(cells[2])),
                why: cells[3].to_owned(),
            };
            match &section {
                Section::Global => mapping.global.push(row),
                Section::Tool(tool) => mapping.tools.get_mut(tool).unwrap().1.push(row),
                Section::Other => panic!("a row outside a table of flags: {line}"),
            }
        }
    }
    mapping
}

/// The name a row gives `arg`: `--long`, else `-s`, else a positional
/// argument's value name.
fn flag_name(arg: &Arg) -> String {
    if let Some(long) = arg.get_long() {
        format!("--{long}")
    } else if let Some(short) = arg.get_short() {
        format!("-{short}")
    } else {
        arg.get_value_names()
            .and_then(|names| names.first())
            .map_or_else(|| arg.get_id().as_str().to_uppercase(), ToString::to_string)
    }
}

/// `args` by the names rows give them, clap's own help and version aside.
fn by_name<'a>(args: impl Iterator<Item = &'a Arg>) -> BTreeMap<String, &'a Arg> {
    args.filter(|arg| !matches!(arg.get_id().as_str(), "help" | "version"))
        .map(|arg| (flag_name(arg), arg))
        .collect()
}

/// What is wrong with `parameter` (`property` in the schema) setting `arg`.
fn disagreement(arg: &Arg, parameter: &str, property: &Value) -> Option<String> {
    let kind = property["type"].as_str().unwrap_or_default();
    let flag = flag_name(arg);
    match arg.get_action() {
        ArgAction::SetTrue | ArgAction::SetFalse if kind != "boolean" => {
            return Some(format!(
                "{flag} is a switch, and {parameter} is not a boolean"
            ));
        }
        ArgAction::Append if kind != "array" => {
            return Some(format!("{flag} repeats, and {parameter} is not an array"));
        }
        ArgAction::Set if kind == "boolean" => {
            return Some(format!(
                "{flag} takes a value, and {parameter} is a boolean"
            ));
        }
        _ => {}
    }
    if !matches!(arg.get_action(), ArgAction::Set | ArgAction::Append) {
        return None;
    }
    let takes: Vec<String> = arg
        .get_possible_values()
        .iter()
        .map(|value| value.get_name().to_owned())
        .collect();
    let choices = property
        .get("enum")
        .or_else(|| property["items"].get("enum"))
        .and_then(Value::as_array);
    match choices {
        Some(choices) => choices
            .iter()
            .find(|choice| !takes.iter().any(|t| choice.as_str() == Some(t)))
            .map(|choice| format!("{parameter} offers {choice}, which {flag} does not take")),
        None if !takes.is_empty() => Some(format!(
            "{flag} takes only {}, and {parameter} has no enum",
            takes.join(", ")
        )),
        None => None,
    }
}

#[test]
fn p3_mcp_04_mapping_names_every_flag_of_each_tools_verb() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../schemas/mcp/MAPPING.md");
    let mapping = read(&std::fs::read_to_string(path).unwrap());
    let command = Cli::command();
    let global = by_name(command.get_arguments().filter(|arg| arg.is_global_set()));
    // The verb each tool runs, from a call without arguments.
    let env = TestEnv::new();
    let roots = Roots::new([env.work_dir()]).unwrap();
    let index_dir = env.index_dir();
    let context = Context {
        roots: &roots,
        index_dir: &index_dir,
    };
    let tools = shipped();
    let mut problems: Vec<String> = Vec::new();
    for tool in &tools {
        if !mapping.tools.contains_key(tool.name()) {
            problems.push(format!("{} has no section", tool.name()));
        }
    }
    for (name, (verb, rows)) in &mapping.tools {
        let Some(tool) = tools.iter().find(|tool| tool.name() == name) else {
            problems.push(format!("{name} has a section and is no tool"));
            continue;
        };
        // A tool that reads a whole scan finds none here, and refuses; its
        // fixture test compares it with its verb.
        match tool.invocation(&Map::new(), &context) {
            Ok(invocation) => assert_eq!(invocation.verb, verb, "{name} runs {}", invocation.verb),
            Err(refusal) => assert_eq!(refusal.code, "scan_not_found", "{name}: {refusal}"),
        }
        let schema: Value = serde_json::from_str(tool.input_schema()).unwrap();
        let properties = schema["properties"].as_object().unwrap();
        let subcommand = command
            .find_subcommand(verb)
            .unwrap_or_else(|| panic!("{name}: no verb {verb}"));
        let mut flags = by_name(subcommand.get_arguments());
        for (flag, arg) in &global {
            flags.entry(flag.clone()).or_insert(arg);
        }
        // A tool's row for a global flag takes the place of the global row.
        let mut table: BTreeMap<&str, &Row> = mapping
            .global
            .iter()
            .map(|row| (row.flag.as_str(), row))
            .collect();
        let mut own: BTreeMap<&str, usize> = BTreeMap::new();
        for row in rows {
            if row.parameter.is_none() && row.why.is_empty() {
                problems.push(format!(
                    "{name}: {} has neither parameter nor reason",
                    row.flag
                ));
            }
            *own.entry(&row.flag).or_default() += 1;
            table.insert(&row.flag, row);
        }
        for (flag, n) in own {
            if n > 1 {
                problems.push(format!("{name}: {flag} has {n} rows"));
            }
        }
        for flag in flags.keys() {
            if !table.contains_key(flag.as_str()) {
                problems.push(format!(
                    "{name}: {verb}'s {flag} is neither exposed nor excluded"
                ));
            }
        }
        for (flag, row) in &table {
            let Some(arg) = flags.get(*flag) else {
                problems.push(format!("{name}: {verb} has no flag {flag}"));
                continue;
            };
            let Some(parameter) = &row.parameter else {
                continue;
            };
            match properties.get(parameter) {
                Some(property) => problems
                    .extend(disagreement(arg, parameter, property).map(|p| format!("{name}: {p}"))),
                None => problems.push(format!(
                    "{name}: {flag}'s parameter {parameter} is not in the input schema"
                )),
            }
        }
        for property in properties.keys() {
            if !table
                .values()
                .any(|row| row.parameter.as_deref() == Some(property))
            {
                problems.push(format!("{name}: {property} sets no flag in MAPPING.md"));
            }
        }
    }
    assert!(problems.is_empty(), "MAPPING.md:\n{}", problems.join("\n"));
}
