//! The advertised shape of the argument slots that accept more than one JSON form.
//!
//! Each token here exists only to carry a `JsonSchema` impl a field points at with
//! `#[schemars(with = …)]`. The field itself stays raw JSON, because the verb behind it owns the
//! refusal: it names the offending value and the verb that would have accepted it, which a
//! deserialization failure could not. see rules: agent-mcp
//!
//! The schemas are inlined rather than referenced: a slot then reads as one keyword with no `$defs`
//! hop for a client to resolve, and an optional slot folds its null into the same `type` list
//! instead of growing a union.

use std::borrow::Cow;

use schemars::{json_schema, JsonSchema, Schema, SchemaGenerator};

/// A **literal** value slot: a number, or an enum symbol string.
///
/// The wire-ref object is deliberately absent — the verbs that write a literal refuse one by name
/// and point at `wire_instrument_input`, so advertising it would invite the call they reject.
pub struct Literal;

impl JsonSchema for Literal {
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

/// One entry of the one-shot add's `inputs` map: a literal, or a wire-ref object — the two forms
/// that path accepts, unlike the literal-only value verbs.
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
