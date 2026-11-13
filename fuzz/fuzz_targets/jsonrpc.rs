//! P3-MCP-01: arbitrary bytes on the MCP transport's input. Whatever they are,
//! the transport frames, parses and answers them without a panic; every line
//! it writes is one well-formed JSON-RPC message; and where the reads split
//! the input changes nothing it writes. The first byte picks the read size;
//! the line limit is lowered so short inputs reach the oversized path.

#![no_main]

use std::io::{self, BufReader, Read, Write};
use std::sync::{Arc, Mutex};

use duscope_mcp::jsonrpc::{Notification, Request, RpcError};
use duscope_mcp::transport::{self, Handler, Outbox, Reader, Responder};
use libfuzzer_sys::fuzz_target;
use serde_json::Value;

/// The longest line read, in place of 1 MiB.
const LIMIT: usize = 512;

#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

impl Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Answers `echo` with its params, leaves `drop` unanswered (the responder
/// answers an internal error) and `abandon` abandoned, and anything else
/// with method not found.
struct Echo;

impl Handler for Echo {
    fn request(&mut self, request: Request, responder: Responder) {
        match request.method.as_str() {
            "echo" => responder
                .respond(Ok(Value::Object(request.params.unwrap_or_default())))
                .unwrap(),
            "drop" => drop(responder),
            "abandon" => responder.abandon(),
            method => responder
                .respond(Err(RpcError::method_not_found(method)))
                .unwrap(),
        }
    }

    fn notification(&mut self, _: Notification) {}
}

/// Hands out its input at most `step` bytes a read.
struct Trickle<'a> {
    rest: &'a [u8],
    step: usize,
}

impl Read for Trickle<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.step.min(buf.len()).min(self.rest.len());
        buf[..n].copy_from_slice(&self.rest[..n]);
        self.rest = &self.rest[n..];
        Ok(n)
    }
}

/// What the transport writes for `input`, read `step` bytes at a time.
fn serve(input: &[u8], step: usize) -> Vec<u8> {
    let out = Buffer::default();
    let reader = BufReader::with_capacity(step, Trickle { rest: input, step });
    transport::run(
        Reader::with_limit(reader, LIMIT),
        &Outbox::new(out.clone()),
        &mut Echo,
    )
    .unwrap();
    out.0.lock().unwrap().clone()
}

/// One line written: a JSON-RPC response or notification, nothing else.
fn check(line: &[u8]) {
    let text = String::from_utf8_lossy(line);
    let message: Value = serde_json::from_slice(line).unwrap_or_else(|e| panic!("{e}: {text}"));
    let object = message
        .as_object()
        .unwrap_or_else(|| panic!("not an object: {text}"));
    assert_eq!(object.get("jsonrpc"), Some(&Value::from("2.0")), "{text}");
    match object.get("id") {
        None | Some(Value::String(_)) => {}
        Some(Value::Number(id)) => assert!(id.is_i64() || id.is_u64(), "{text}"),
        Some(_) => panic!("a bad id: {text}"),
    }
    let kinds = ["result", "error", "method"]
        .iter()
        .filter(|key| object.contains_key(**key))
        .count();
    assert_eq!(kinds, 1, "{text}");
    if let Some(error) = object.get("error") {
        assert!(
            error["code"].is_i64() && error["message"].is_string(),
            "{text}"
        );
    } else if object.contains_key("result") {
        assert!(object.contains_key("id"), "a result without an id: {text}");
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((&split, input)) = data.split_first() else {
        return;
    };
    let whole = serve(input, input.len().max(1));
    let step = usize::from(split % 64) + 1;
    assert_eq!(whole, serve(input, step), "reads of {step} bytes");
    assert!(whole.is_empty() || whole.ends_with(b"\n"));
    for line in whole.split(|&b| b == b'\n').filter(|line| !line.is_empty()) {
        check(line);
    }
});
