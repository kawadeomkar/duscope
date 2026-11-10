//! P3-MCP-03: the registry: names, schemas and what `tools/call` answers,
//! with a runner that records what it is asked to run.

use std::sync::Mutex;

use duscope_testkit::TestEnv;
use serde_json::json;

use super::*;

/// A tool whose parts the test sets.
struct Fake {
    name: &'static str,
    schema: &'static str,
    annotations: Annotations,
    interactive: bool,
}

const READ_ONLY: Annotations = Annotations {
    read_only: true,
    destructive: false,
    idempotent: true,
};

const SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "path": { "type": "string", "description": "A path, with  two spaces and a \" quote" },
    "limit": { "type": "integer", "minimum": 1, "maximum": 100 },
    "size": { "type": "string", "enum": ["allocated", "apparent"] },
    "fields": { "type": "array", "items": { "type": "string", "pattern": "^[a-z_]+$" }, "maxLength": 3 },
    "detail": { "type": "boolean" }
  },
  "required": ["path"],
  "additionalProperties": false
}"#;

fn fake(name: &'static str) -> Fake {
    Fake {
        name,
        schema: SCHEMA,
        annotations: READ_ONLY,
        interactive: false,
    }
}

impl Tool for Fake {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        "A fake tool."
    }

    fn input_schema(&self) -> &'static str {
        self.schema
    }

    fn annotations(&self) -> Annotations {
        self.annotations
    }

    fn requires_user_interaction(&self) -> bool {
        self.interactive
    }

    fn invocation(
        &self,
        arguments: &Map<String, Value>,
        context: &Context<'_>,
    ) -> Result<Invocation, Refusal> {
        let path = path_argument(arguments, "path", None, context.roots)?;
        Ok(Invocation {
            verb: "top",
            args: vec![path.into()],
        })
    }

    fn summary(&self, envelope: &Value) -> String {
        format!("rows: {}\nend", envelope["returned_count"])
    }
}

/// Answers every run with `json`, refuses with the refusal's code, and
/// records what it ran.
struct Recorder {
    json: &'static str,
    ok: bool,
    ran: Mutex<Vec<Invocation>>,
    refused: Mutex<Vec<Refusal>>,
}

impl Recorder {
    fn new(json: &'static str, ok: bool) -> Recorder {
        Recorder {
            json,
            ok,
            ran: Mutex::new(Vec::new()),
            refused: Mutex::new(Vec::new()),
        }
    }
}

impl Runner for Recorder {
    fn run(&self, invocation: &Invocation) -> Outcome {
        self.ran.lock().unwrap().push(invocation.clone());
        Outcome {
            json: self.json.to_owned(),
            ok: self.ok,
        }
    }

    fn refuse(&self, refusal: &Refusal) -> Outcome {
        self.refused.lock().unwrap().push(refusal.clone());
        Outcome {
            json: json!({
                "ok": false,
                "error": { "code": refusal.code, "message": refusal.message, "hint": refusal.hint }
            })
            .to_string(),
            ok: false,
        }
    }
}

struct Setup {
    env: TestEnv,
    roots: Roots,
}

impl Setup {
    fn new() -> Setup {
        let env = TestEnv::new();
        let roots = Roots::new([env.work_dir()]).unwrap();
        Setup { env, roots }
    }

    fn call(
        &self,
        registry: &Registry,
        runner: &Recorder,
        params: &Value,
    ) -> Result<CallResult, RpcError> {
        let context = Context {
            roots: &self.roots,
            index_dir: &self.env.index_dir(),
        };
        registry.call(params.as_object(), &context, runner)
    }

    fn root(&self) -> String {
        self.roots.paths()[0].display().to_string()
    }
}

fn registry_with(tools: Vec<Fake>) -> Registry {
    let mut registry = Registry::new(24_000, false);
    for tool in tools {
        registry.register(Box::new(tool)).unwrap();
    }
    registry
}

#[test]
fn p3_mcp_03_tool_names_follow_the_clients_limits() {
    let mut registry = Registry::new(24_000, false);
    let long: &'static str = Box::leak("t".repeat(MAX_NAME_CHARS + 1).into_boxed_str());
    for name in ["", "has space", "dot.ted", "slash/ed", long] {
        let error = registry.register(Box::new(fake(name))).unwrap_err();
        assert!(error.message.contains("name"), "{name:?}: {error}");
    }
    let longest: &'static str = Box::leak("t".repeat(MAX_NAME_CHARS).into_boxed_str());
    registry.register(Box::new(fake(longest))).unwrap();
    registry.register(Box::new(fake("duscope_top-2"))).unwrap();
    let twice = registry
        .register(Box::new(fake("duscope_top-2")))
        .unwrap_err();
    assert!(twice.message.contains("twice"), "{twice}");
}

#[test]
fn p3_mcp_03_a_schema_uses_only_the_keywords_the_server_checks() {
    let cases = [
        (
            r#"{"type":"object","additionalProperties":false,"oneOf":[]}"#,
            "oneOf",
        ),
        (
            r#"{"type":"object","properties":{}}"#,
            "additionalProperties",
        ),
        (
            r#"{"type":"array","additionalProperties":false}"#,
            "type must be object",
        ),
        (
            r#"{"type":"object","additionalProperties":false,"properties":{"a":{"type":"string","format":"uri"}}}"#,
            "format",
        ),
        (
            r#"{"type":"object","additionalProperties":false,"properties":{"a":{"type":"array"}}}"#,
            "needs items",
        ),
        (
            r#"{"type":"object","additionalProperties":false,"required":["b"],"properties":{"a":{"type":"string"}}}"#,
            "not a property",
        ),
        (
            r#"{"type":"object","additionalProperties":false,"properties":{"a":{"type":"string","pattern":"("}}}"#,
            "pattern",
        ),
        ("not json", "not JSON"),
    ];
    for (schema, expected) in cases {
        let mut registry = Registry::new(24_000, false);
        let tool = Fake {
            schema,
            ..fake("t")
        };
        let error = registry.register(Box::new(tool)).unwrap_err();
        assert!(error.message.contains(expected), "{schema}: {error}");
    }
}

#[test]
fn p3_mcp_03_a_tool_that_deletes_is_listed_only_under_allow_delete() {
    let deletes = || Fake {
        annotations: Annotations {
            read_only: false,
            destructive: true,
            idempotent: false,
        },
        interactive: true,
        ..fake("duscope_delete")
    };
    let mut read_only = Registry::new(24_000, false);
    read_only.register(Box::new(deletes())).unwrap();
    assert_eq!(read_only.names().count(), 0);
    let mut allowed = Registry::new(24_000, true);
    allowed.register(Box::new(deletes())).unwrap();
    let listed = serde_json::to_value(allowed.list()).unwrap();
    assert_eq!(listed[0]["annotations"]["destructiveHint"], true);
    assert_eq!(
        listed[0]["_meta"]["anthropic/requiresUserInteraction"],
        true
    );
}

#[test]
fn p3_mcp_03_tools_list_carries_names_annotations_schemas_and_the_size_cap() {
    let mut registry = Registry::new(1234, false);
    registry.register(Box::new(fake("b_tool"))).unwrap();
    registry.register(Box::new(fake("a_tool"))).unwrap();
    let text = serde_json::to_string(&registry.list()).unwrap();
    let expected = concat!(
        r#"[{"name":"a_tool","description":"A fake tool.","inputSchema":"#,
        r#"{"type":"object","properties":{"path":{"type":"string","description":"A path, with  two spaces and a \" quote"},"#,
        r#""limit":{"type":"integer","minimum":1,"maximum":100},"size":{"type":"string","enum":["allocated","apparent"]},"#,
        r#""fields":{"type":"array","items":{"type":"string","pattern":"^[a-z_]+$"},"maxLength":3},"detail":{"type":"boolean"}},"#,
        r#""required":["path"],"additionalProperties":false},"#,
        r#""annotations":{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false},"#,
        r#""_meta":{"anthropic/maxResultSizeChars":1234}},"#,
    );
    assert!(text.starts_with(expected), "{text}");
    let listed: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(listed[1]["name"], "b_tool", "sorted by name");
}

#[test]
fn p3_mcp_03_arguments_are_checked_against_the_schema() {
    let setup = Setup::new();
    let registry = registry_with(vec![fake("t")]);
    let root = setup.root();
    let cases = [
        (json!({}), "the argument path is required"),
        (json!({"path": root, "other": 1}), "unknown argument other"),
        (json!({"path": 3}), "path must be a string"),
        (
            json!({"path": root, "limit": 0}),
            "limit must be an integer from 1 to 100",
        ),
        (
            json!({"path": root, "limit": 2.5}),
            "limit must be an integer",
        ),
        (
            json!({"path": root, "limit": "5"}),
            "limit must be an integer",
        ),
        (
            json!({"path": root, "size": "big"}),
            "one of \"allocated\", \"apparent\"",
        ),
        (
            json!({"path": root, "fields": ["Name"]}),
            "matching ^[a-z_]+$",
        ),
        (
            json!({"path": root, "fields": ["a", "b", "c", "d"]}),
            "at most 3 long",
        ),
        (
            json!({"path": root, "fields": "name"}),
            "fields must be an array",
        ),
        (
            json!({"path": root, "detail": "yes"}),
            "detail must be true or false",
        ),
    ];
    for (arguments, expected) in cases {
        let runner = Recorder::new("{}", true);
        let result = setup
            .call(
                &registry,
                &runner,
                &json!({"name": "t", "arguments": arguments}),
            )
            .unwrap();
        assert!(result.is_error(), "{arguments}");
        let refused = runner.refused.lock().unwrap();
        assert_eq!(refused.len(), 1, "{arguments}");
        assert_eq!(refused[0].code, "usage");
        assert!(
            refused[0].message.contains(expected),
            "{arguments}: {}",
            refused[0].message
        );
        assert!(runner.ran.lock().unwrap().is_empty(), "nothing ran");
    }
}

#[test]
fn p3_mcp_03_a_call_carries_the_verbs_envelope_byte_for_byte() {
    let setup = Setup::new();
    let registry = registry_with(vec![fake("t")]);
    let envelope = r#"{"ok":true,"z_last":1,"a_first":2,"returned_count":7,"entries":[]}"#;
    let runner = Recorder::new(envelope, true);
    let result = setup
        .call(
            &registry,
            &runner,
            &json!({"name": "t", "arguments": {"path": setup.root(), "limit": 5}}),
        )
        .unwrap();
    assert!(!result.is_error());
    assert_eq!(result.structured_content(), envelope);
    let text = serde_json::to_string(&result).unwrap();
    assert_eq!(
        text,
        format!(
            r#"{{"content":[{{"type":"text","text":"rows: 7 end"}}],"structuredContent":{envelope},"isError":false}}"#
        )
    );
    let ran = runner.ran.lock().unwrap();
    assert_eq!(
        ran.as_slice(),
        [Invocation {
            verb: "top",
            args: vec![setup.roots.paths()[0].clone().into()],
        }]
    );
}

#[test]
fn p3_mcp_03_a_failed_verb_is_an_error_result_with_actionable_text() {
    let setup = Setup::new();
    let registry = registry_with(vec![fake("t")]);
    let envelope = r#"{"ok":false,"error":{"code":"scan_not_found","message":"no saved scan holds /x","hint":"Call duscope_scan first"}}"#;
    let runner = Recorder::new(envelope, false);
    let result = setup
        .call(
            &registry,
            &runner,
            &json!({"name": "t", "arguments": {"path": setup.root()}}),
        )
        .unwrap();
    assert!(result.is_error());
    assert_eq!(result.structured_content(), envelope);
    let value = serde_json::to_value(&result).unwrap();
    assert_eq!(
        value["content"][0]["text"],
        "no saved scan holds /x. Call duscope_scan first"
    );
}

#[test]
fn p3_mcp_03_a_path_outside_the_roots_is_refused_before_the_verb_runs() {
    let setup = Setup::new();
    let registry = registry_with(vec![fake("t")]);
    let runner = Recorder::new("{}", true);
    let result = setup
        .call(
            &registry,
            &runner,
            &json!({"name": "t", "arguments": {"path": "/"}}),
        )
        .unwrap();
    assert!(result.is_error());
    assert!(runner.ran.lock().unwrap().is_empty());
    assert_eq!(runner.refused.lock().unwrap()[0].code, "refused_path");
    let value = serde_json::to_value(&result).unwrap();
    assert_eq!(value["structuredContent"]["error"]["code"], "refused_path");
}

#[test]
fn p3_mcp_03_an_unknown_tool_or_malformed_params_are_protocol_errors() {
    let setup = Setup::new();
    let registry = registry_with(vec![fake("t")]);
    let runner = Recorder::new("{}", true);
    for params in [
        json!({"name": "nope", "arguments": {}}),
        json!({"arguments": {}}),
        json!({"name": 3}),
        json!({"name": "t", "arguments": []}),
        json!({"name": "t", "arguments": "path=/"}),
    ] {
        let error = setup.call(&registry, &runner, &params).unwrap_err();
        assert_eq!(error.code, crate::jsonrpc::INVALID_PARAMS, "{params}");
    }
    let context = Context {
        roots: &setup.roots,
        index_dir: &setup.env.index_dir(),
    };
    let error = registry.call(None, &context, &runner).unwrap_err();
    assert_eq!(error.code, crate::jsonrpc::INVALID_PARAMS);
    // Absent or null arguments are none.
    let runner = Recorder::new("{}", true);
    let registry = registry_with(vec![fake("t")]);
    for params in [
        json!({"name": "t"}),
        json!({"name": "t", "arguments": null}),
    ] {
        let result = setup.call(&registry, &runner, &params).unwrap();
        assert!(result.is_error(), "path is required");
    }
}

#[test]
fn p3_mcp_03_an_envelope_on_several_lines_goes_out_on_one() {
    let setup = Setup::new();
    let registry = registry_with(vec![fake("t")]);
    let runner = Recorder::new("{\n  \"ok\": true,\n  \"returned_count\": 1\n}", true);
    let result = setup
        .call(
            &registry,
            &runner,
            &json!({"name": "t", "arguments": {"path": setup.root()}}),
        )
        .unwrap();
    assert!(!result.structured_content().contains('\n'));
    assert_eq!(
        serde_json::from_str::<Value>(result.structured_content()).unwrap(),
        json!({"ok": true, "returned_count": 1})
    );
}

#[test]
fn p3_mcp_03_minify_keeps_strings_as_written() {
    assert_eq!(
        minify("{ \"a b\" : [ 1 ,\n 2 ], \"c\":\"x \\\" y\" }"),
        r#"{"a b":[1,2],"c":"x \" y"}"#
    );
}

#[test]
fn p5_sec_05_each_tool_names_its_verb_before_its_arguments_are_checked() {
    let env = TestEnv::new();
    let roots = Roots::new([env.work_dir()]).unwrap();
    let index_dir = env.index_dir();
    let context = Context {
        roots: &roots,
        index_dir: &index_dir,
    };
    for tool in shipped() {
        let name = tool.name();
        let expected = match name {
            "duscope_delete" => "clean".to_owned(),
            other => other.strip_prefix("duscope_").unwrap().replace('_', "-"),
        };
        assert_eq!(tool.verb(), Some(expected.as_str()), "{name}");
        // Where no argument is needed, the command line says the same.
        if let Ok(invocation) = tool.invocation(&Map::new(), &context) {
            assert_eq!(invocation.verb, expected, "{name}");
        }
    }
}

#[test]
fn p5_sec_05_the_largest_real_requests_fit_in_a_message() {
    let limit = crate::transport::MAX_MESSAGE_BYTES;
    let line = |tool: &str, arguments: Value| {
        json!({
            "jsonrpc": "2.0",
            "id": u64::MAX,
            "method": "tools/call",
            "params": {
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {},
                    "io.modelcontextprotocol/clientInfo": {"name": "client", "version": "1.0.0"},
                },
                "name": tool,
                "arguments": arguments,
            },
        })
        .to_string()
        .len()
    };
    // duscope_delete: as many items as its schema allows, each with the
    // longest handle and the largest size.
    let schema: Value = serde_json::from_str(delete::Delete.input_schema()).unwrap();
    let items = &schema["properties"]["items"];
    let count = usize::try_from(items["maxItems"].as_u64().unwrap()).unwrap();
    let longest = items["items"]["properties"]["handle"]["maxLength"]
        .as_u64()
        .unwrap();
    let handle = "h".repeat(usize::try_from(longest).unwrap());
    let item = json!({"handle": handle, "expect_bytes": u64::MAX});
    let delete = line(
        "duscope_delete",
        json!({"items": vec![item; count], "dry_run": false}),
    );
    assert!(delete < limit, "duscope_delete: {delete} bytes of {limit}");
    // duscope_query: the sandbox's longest statement (100,000 bytes, its
    // SQLITE_LIMIT_SQL_LENGTH), each byte one JSON writes in six.
    let sql = "\u{1}".repeat(100_000);
    let query = line("duscope_query", json!({"sql": sql, "scan_id": "latest"}));
    assert!(query < limit, "duscope_query: {query} bytes of {limit}");
}
