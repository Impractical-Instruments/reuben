//! One dispatcher over every non-engine roster verb: a verb named by string, its arguments as
//! JSON, its answer as JSON. The `match` in [`call`] is the only list of verbs a host dispatches
//! through; it names [`names`] symbols, so removing a roster entry breaks the build here.

use alloc::format;
use alloc::string::{String, ToString};

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use super::*;
use crate::tools::names;

/// What the caller must do with a verb's result beyond relaying it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallKind {
    /// Nothing: the verb only read.
    Read,
    /// The verb edited an existing document: stage and plan `source`.
    Edit { source: String },
    /// The verb wrote a document that did not exist: allow a fresh file at `source`.
    Create { source: String },
}

/// A verb's answer as the wire carries it.
#[derive(Debug, Clone, PartialEq)]
pub struct Called {
    pub output: Value,
    pub summary: String,
    pub kind: CallKind,
}

fn read<A>(_: &A) -> CallKind {
    CallKind::Read
}

fn run<A, T>(
    args: Value,
    kind: impl FnOnce(&A) -> CallKind,
    verb: impl FnOnce(&A) -> Result<Answer<T>, Refusal>,
) -> Result<Called, Refusal>
where
    A: DeserializeOwned,
    T: Serialize,
{
    let args: A = serde_json::from_value(args).map_err(|e| Refusal::new(e.to_string()))?;
    let kind = kind(&args);
    let answer = verb(&args)?;
    let output = serde_json::to_value(answer.output).map_err(|e| Refusal::new(e.to_string()))?;
    Ok(Called {
        output,
        summary: answer.summary,
        kind,
    })
}

/// Run the non-engine roster verb `tool` with JSON `args` against `store`.
///
/// Refuses an engine verb (the engine serves those), a name the roster does not carry, and
/// arguments that do not deserialize into the verb's argument struct.
pub fn call<R: Resources>(
    tool: &str,
    args: Value,
    store: &DocumentHistory<R>,
) -> Result<Called, Refusal> {
    let res: &dyn Resources = store;
    macro_rules! edit {
        ($args:ident, $ty:ty, $verb:ident) => {
            run::<$ty, _>(
                $args,
                |a| CallKind::Edit {
                    source: a.source.clone(),
                },
                |a| $verb(a, res),
            )
        };
    }
    match tool {
        names::DESCRIBE_OPERATORS => run::<DescribeOperators, _>(args, read, describe_operators),
        names::DESCRIBE_INSTRUMENT => {
            run::<DescribeInstrument, _>(args, read, |a| describe_instrument(a, res))
        }
        names::VALIDATE_INSTRUMENT => {
            run::<ValidateInstrument, _>(args, read, |a| validate_instrument(a, res))
        }
        names::DESCRIBE_BOUNDARY => {
            run::<DescribeBoundary, _>(args, read, |a| describe_boundary(a, res))
        }
        names::WIRE_COMPATIBILITY => {
            run::<WireCompatibility, _>(args, read, |a| wire_compatibility(a, res))
        }
        names::READ_INSTRUMENT => run::<ReadInstrument, _>(args, read, |a| read_instrument(a, res)),

        names::NEW_INSTRUMENT => run::<NewInstrument, _>(
            args,
            |a| CallKind::Create {
                source: a.source.clone(),
            },
            |a| new_instrument(a, res),
        ),
        names::COPY_INSTRUMENT => run::<CopyInstrument, _>(
            args,
            |a| CallKind::Create {
                source: a.to.clone(),
            },
            |a| copy_instrument(a, res),
        ),
        names::RESTORE_INSTRUMENT => run::<RestoreInstrument, _>(
            args,
            |a| CallKind::Edit {
                source: a.source.clone(),
            },
            |a| restore_instrument(a, store),
        ),

        names::SET_INSTRUMENT_NAME => edit!(args, SetInstrumentName, set_instrument_name),
        names::SET_INSTRUMENT_DESCRIPTION => {
            edit!(args, SetInstrumentDescription, set_instrument_description)
        }
        names::ADD_INSTRUMENT_NODE => edit!(args, AddInstrumentNode, add_instrument_node),
        names::REMOVE_INSTRUMENT_NODE => edit!(args, RemoveInstrumentNode, remove_instrument_node),
        names::RENAME_INSTRUMENT_NODE => edit!(args, RenameInstrumentNode, rename_instrument_node),
        names::SET_INSTRUMENT_NODE_DESCRIPTION => edit!(
            args,
            SetInstrumentNodeDescription,
            set_instrument_node_description
        ),
        names::SET_INSTRUMENT_INPUT => edit!(args, SetInstrumentInput, set_instrument_input),
        names::SET_INSTRUMENT_INPUTS_BY_INTENT => edit!(
            args,
            SetInstrumentInputsByIntent,
            set_instrument_inputs_by_intent
        ),
        names::WIRE_INSTRUMENT_INPUT => edit!(args, WireInstrumentInput, wire_instrument_input),
        names::UNWIRE_INSTRUMENT_INPUT => {
            edit!(args, UnwireInstrumentInput, unwire_instrument_input)
        }
        names::SET_INSTRUMENT_CONSTANT => {
            edit!(args, SetInstrumentConstant, set_instrument_constant)
        }
        names::REMOVE_INSTRUMENT_CONSTANT => {
            edit!(args, RemoveInstrumentConstant, remove_instrument_constant)
        }
        names::ADD_INSTRUMENT_INTERFACE_INPUT => edit!(
            args,
            AddInstrumentInterfaceInput,
            add_instrument_interface_input
        ),
        names::ADD_INSTRUMENT_INTERFACE_OUTPUT => edit!(
            args,
            AddInstrumentInterfaceOutput,
            add_instrument_interface_output
        ),
        names::REMOVE_INSTRUMENT_INTERFACE_INPUT => edit!(
            args,
            RemoveInstrumentInterfacePipe,
            remove_instrument_interface_input
        ),
        names::REMOVE_INSTRUMENT_INTERFACE_OUTPUT => edit!(
            args,
            RemoveInstrumentInterfacePipe,
            remove_instrument_interface_output
        ),
        names::SET_INSTRUMENT_INTERFACE_INPUT_META => edit!(
            args,
            SetInstrumentInterfaceInputMeta,
            set_instrument_interface_input_meta
        ),
        names::SET_INSTRUMENT_INTERFACE_OUTPUT_META => edit!(
            args,
            SetInstrumentInterfaceOutputMeta,
            set_instrument_interface_output_meta
        ),
        names::CLEAR_INSTRUMENT_INTERFACE_META => edit!(
            args,
            ClearInstrumentInterfaceMeta,
            clear_instrument_interface_meta
        ),
        names::WIRE_INSTRUMENT_INTERFACE_OUTPUT => edit!(
            args,
            WireInstrumentInterfaceOutput,
            wire_instrument_interface_output
        ),
        names::ADD_INSTRUMENT_RESOURCE => {
            edit!(args, AddInstrumentResource, add_instrument_resource)
        }
        names::REMOVE_INSTRUMENT_RESOURCE => {
            edit!(args, RemoveInstrumentResource, remove_instrument_resource)
        }
        names::SET_INSTRUMENT_RESOURCE => {
            edit!(args, SetInstrumentResource, set_instrument_resource)
        }
        names::SET_INSTRUMENT_NODE_RESOURCE => edit!(
            args,
            SetInstrumentNodeResource,
            set_instrument_node_resource
        ),
        names::SET_INSTRUMENT_LAYOUT => edit!(args, SetInstrumentLayout, set_instrument_layout),

        names::SEND_LIVE_CONTROLS
        | names::GET_ENGINE_STATUS
        | names::SWAP_INSTRUMENT
        | names::GET_CURRENT_INSTRUMENT
        | names::GET_ENGINE_DIAGNOSTICS
        | names::GET_ENGINE_METERS => Err(Refusal::new(format!(
            "`{tool}` is an engine verb, not served by call"
        ))),
        other => Err(Refusal::new(format!("unknown tool `{other}`"))),
    }
}

#[cfg(test)]
mod tests {
    use alloc::collections::BTreeMap;
    use alloc::string::ToString;
    use core::cell::RefCell;

    use serde_json::json;

    use super::*;
    use crate::tools::{ContractKind, CONTRACTS};

    #[derive(Default)]
    struct MemoryStore {
        texts: RefCell<BTreeMap<String, String>>,
    }

    impl Resources for MemoryStore {
        fn read_samples(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
            Err(ResolveError::NotFound(source.to_string()))
        }

        fn read_text(&self, source: &str) -> Result<String, ResolveError> {
            self.texts
                .borrow()
                .get(source)
                .cloned()
                .ok_or_else(|| ResolveError::NotFound(source.to_string()))
        }

        fn write_text(&self, source: &str, text: &str) -> Result<(), ResolveError> {
            self.texts
                .borrow_mut()
                .insert(source.to_string(), text.to_string());
            Ok(())
        }

        fn canonical(&self, source: &str, _referrer: Option<&str>) -> String {
            source.to_string()
        }
    }

    fn store_with_doc_at_a() -> DocumentHistory<MemoryStore> {
        let store = DocumentHistory::new(MemoryStore::default());
        let made = call(
            names::NEW_INSTRUMENT,
            json!({"source": "a", "name": "t"}),
            &store,
        )
        .expect("a seed document");
        assert_eq!(made.output["written"], true, "{:?}", made.output);
        store
    }

    #[test]
    fn every_non_engine_contract_dispatches() {
        let store = DocumentHistory::new(MemoryStore::default());
        for contract in CONTRACTS.iter().filter(|c| c.kind != ContractKind::Engine) {
            if let Err(refusal) = call(contract.name, json!({}), &store) {
                assert!(
                    !refusal.message.contains("unknown tool"),
                    "`{}` has no dispatch arm: {refusal}",
                    contract.name
                );
            }
        }
    }

    #[test]
    fn engine_contract_is_refused() {
        let store = DocumentHistory::new(MemoryStore::default());
        let refusal = call(names::SWAP_INSTRUMENT, json!({}), &store).unwrap_err();
        assert!(refusal.message.contains("not served by call"), "{refusal}");
    }

    #[test]
    fn unknown_and_unlisted_names_are_refused() {
        let store = DocumentHistory::new(MemoryStore::default());
        for name in ["no_such_verb", "hash_instrument"] {
            let refusal = call(name, json!({}), &store).unwrap_err();
            assert!(refusal.message.contains("unknown tool"), "{refusal}");
        }
    }

    #[test]
    fn edit_is_classified_with_its_source() {
        let store = store_with_doc_at_a();
        let called = call(
            names::SET_INSTRUMENT_NAME,
            json!({"source": "a", "name": "x"}),
            &store,
        )
        .unwrap();
        assert_eq!(called.kind, CallKind::Edit { source: "a".into() });
        assert_eq!(called.output["written"], true);
    }

    #[test]
    fn create_is_classified() {
        let store = DocumentHistory::new(MemoryStore::default());
        let called = call(
            names::NEW_INSTRUMENT,
            json!({"source": "b", "name": "t"}),
            &store,
        )
        .unwrap();
        assert_eq!(called.kind, CallKind::Create { source: "b".into() });
    }

    #[test]
    fn copy_is_a_create_at_its_destination() {
        let store = store_with_doc_at_a();
        let called = call(
            names::COPY_INSTRUMENT,
            json!({"from": "a", "to": "c"}),
            &store,
        )
        .unwrap();
        assert_eq!(called.kind, CallKind::Create { source: "c".into() });
    }

    #[test]
    fn a_read_is_classified_read() {
        let store = store_with_doc_at_a();
        let called = call(names::VALIDATE_INSTRUMENT, json!({"source": "a"}), &store).unwrap();
        assert_eq!(called.kind, CallKind::Read);
    }

    #[test]
    fn malformed_arguments_carry_serdes_message() {
        let store = DocumentHistory::new(MemoryStore::default());
        let refusal = call(names::SET_INSTRUMENT_NAME, json!({"source": 3}), &store).unwrap_err();
        assert!(refusal.message.contains("invalid type"), "{refusal}");
    }
}
