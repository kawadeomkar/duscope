//! `--fields` and `--detail`: which fields of each row a list writes, and
//! in what order, under JSON, `ndjson` and `--plain`. Tables keep their
//! columns.

use std::borrow::Cow;

use duscope_core::envelope::{Envelope, Listing};
use serde::Serialize;
use serde::ser::SerializeMap;
use serde_json::{Map, Value};

use super::Row;
use crate::verbs::{Exit, Failure};

/// The fields a scan with `--attrs basic` holds no value for.
const ATTR_FIELDS: [&str; 5] = ["atime", "btime", "mode", "uid", "gid"];

/// What `--fields` and `--detail` ask for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fields {
    /// `--fields help`: list the verb's fields, and nothing else.
    Help,
    /// `--detail`: every field.
    Detail,
    /// `--fields a,b,c`: these, in this order.
    Named(Vec<String>),
}

impl Fields {
    /// The flags' choice; `None` keeps each format's own fields.
    pub fn from_flags(fields: Option<&str>, detail: bool) -> Option<Fields> {
        match fields.map(str::trim) {
            Some("help") => Some(Fields::Help),
            Some(list) => Some(Fields::Named(
                list.split(',')
                    .map(|f| f.trim().to_owned())
                    .filter(|f| !f.is_empty())
                    .collect(),
            )),
            None => detail.then_some(Fields::Detail),
        }
    }
}

/// The fields `fields` names for rows of type `T`: every one with
/// `--detail`, those named otherwise; `unknown_field` for a name `T` does
/// not have.
pub fn chosen<T: Row>(fields: &Fields, verb: &str) -> Result<Option<Vec<&'static str>>, Failure> {
    match fields {
        Fields::Help => Ok(None),
        Fields::Detail => Ok(Some(T::NAMES.to_vec())),
        Fields::Named(names) => names
            .iter()
            .map(|name| {
                T::NAMES
                    .iter()
                    .copied()
                    .find(|known| known == name)
                    .ok_or_else(|| unknown::<T>(name, verb))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
    }
}

/// `unknown_field`: `name` is not one of `T`'s fields.
fn unknown<T: Row>(name: &str, verb: &str) -> Failure {
    Failure::new(
        Exit::Usage,
        "unknown_field",
        format!(
            "{verb} rows have no field {name:?}; they have {}",
            T::NAMES.join(", ")
        ),
    )
    .hint(format!(
        "Run duscope {verb} --fields help to list the fields"
    ))
}

/// `attrs_not_collected` when `chosen` names a field a basic scan holds no
/// value for, and the scan is one (`attrs` its `scan.attrs`).
pub fn check_attrs(chosen: &[&str], attrs: &str, named: bool) -> Result<(), Failure> {
    match chosen.iter().find(|f| ATTR_FIELDS.contains(f)) {
        Some(field) if named && attrs == "basic" => Err(Failure::new(
            Exit::Runtime,
            "attrs_not_collected",
            format!("--fields {field} needs --attrs full, which this scan did not collect"),
        )
        .hint("Rerun the command with --attrs full")),
        _ => Ok(()),
    }
}

/// A row as an object of its fields.
pub fn object(row: &impl Serialize) -> Map<String, Value> {
    match serde_json::to_value(row) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

/// A row written with the chosen fields only, in their order; a field it
/// leaves out (`name_raw` for a UTF-8 name) stays out.
#[derive(Debug)]
pub struct Selected<'a> {
    row: Map<String, Value>,
    fields: &'a [&'static str],
}

impl Serialize for Selected<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        for field in self.fields {
            if let Some(value) = self.row.get(*field) {
                map.serialize_entry(field, value)?;
            }
        }
        map.end()
    }
}

/// `envelope` with each row cut to `fields`.
pub fn select<'f, T: Row>(
    envelope: &Envelope<T>,
    fields: &'f [&'static str],
) -> Envelope<Selected<'f>> {
    let entries = envelope
        .entries
        .iter()
        .map(|row| Selected {
            row: object(row),
            fields,
        })
        .collect();
    Envelope {
        ok: envelope.ok,
        schema_version: envelope.schema_version,
        tool: envelope.tool,
        scan: envelope.scan.clone(),
        query: envelope.query.clone(),
        entries,
        returned_count: envelope.returned_count,
        total_count: envelope.total_count,
        truncated: envelope.truncated,
        next_cursor: envelope.next_cursor.clone(),
        warnings: envelope.warnings.clone(),
        next: envelope.next.clone(),
    }
}

/// `listing` with each row cut to `fields`.
pub fn select_listing<'f, T: Row>(
    listing: &Listing<T>,
    fields: &'f [&'static str],
) -> Listing<Selected<'f>> {
    let entries = listing
        .entries
        .iter()
        .map(|row| Selected {
            row: object(row),
            fields,
        })
        .collect();
    Listing {
        ok: listing.ok,
        schema_version: listing.schema_version,
        tool: listing.tool,
        query: listing.query.clone(),
        entries,
        returned_count: listing.returned_count,
        total_count: listing.total_count,
        truncated: listing.truncated,
        next_cursor: listing.next_cursor.clone(),
        warnings: listing.warnings.clone(),
        next: listing.next.clone(),
    }
}

/// A row's `--plain` record of `fields`: a field `--plain` writes anyway
/// as it writes it (bytes as integers, paths raw), another from its JSON
/// value (a string as it is, a list joined with commas, null empty).
pub fn plain<'r, T: Row>(row: &'r T, fields: &[&'static str]) -> Vec<Cow<'r, [u8]>> {
    let own = row.plain();
    let mut own: Vec<Option<Cow<'r, [u8]>>> = own.into_iter().map(Some).collect();
    let mut json: Option<Map<String, Value>> = None;
    fields
        .iter()
        .map(|field| {
            if let Some(i) = T::FIELDS.iter().position(|f| f == field)
                && let Some(Some(value)) = own.get_mut(i).map(Option::take)
            {
                return value;
            }
            let json = json.get_or_insert_with(|| object(row));
            Cow::Owned(text(json.get(*field).unwrap_or(&Value::Null)).into_bytes())
        })
        .collect()
}

/// A JSON value as `--plain` writes it.
fn text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().map(text).collect::<Vec<_>>().join(","),
        other => other.to_string(),
    }
}
