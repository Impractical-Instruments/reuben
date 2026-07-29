//! The advertised shape of the argument slots that accept more than one JSON form.
//!
//! Each token here exists only to carry a `JsonSchema` impl a field points at with
//! `#[schemars(with = …)]`; the field itself stays raw JSON. see rules: agent-mcp

use std::borrow::Cow;

use schemars::{json_schema, JsonSchema, Schema, SchemaGenerator};

/// A **literal** value slot: a number, or an enum symbol string. No wire-ref object — the verbs
/// that write a literal refuse one by name.
pub struct Literal;

impl JsonSchema for Literal {
    // Inlined, so an `Option` of this folds its null into the same `type` list rather than wrapping
    // the whole thing in a union.
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        "Literal".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": ["number", "string"] })
    }
}

/// One entry of the one-shot add's `inputs` map: a literal, or a wire-ref object.
pub struct NodeInput;

impl JsonSchema for NodeInput {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        "NodeInput".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "anyOf": [
                { "type": ["number", "string"] },
                {
                    "type": "object",
                    "properties": {
                        "from": {
                            "type": "string",
                            "description": "The source port: `/node.port`, or `/node` for a sole-output source."
                        }
                    },
                    "required": ["from"]
                }
            ]
        })
    }
}
