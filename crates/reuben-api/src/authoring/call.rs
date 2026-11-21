//! One dispatcher over every non-engine roster verb: a verb named by string, its arguments as
//! JSON, its answer as JSON. The `match` in [`call`] is the only list of verbs a host dispatches
//! through; it names [`names`] symbols, so removing a roster entry breaks the build here.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use super::history::restore_instrument_through;
use super::*;
use crate::resources::{ScopedTo, Undecoded};
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

/// A file a call's arguments make the server read or write, and how its name is spelled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Named {
    /// A document, spelled as the store spells one (`canonical(name, None)`).
    Document(String),
    /// A resource or nested reference written into `document`, spelled relative to it
    /// (`canonical(name, Some(canonical(document, None)))`).
    Reference { document: String, name: String },
}

/// The files the non-engine roster verb `tool` would touch given `args`, or `None` when `tool` is
/// not one. Arguments that are not file names (node addresses, port paths, resource ids, a
/// restore's content hash) are never listed, and an argument that is missing or not a string is
/// left for [`call`] to refuse.
///
/// Beside [`call`] because the two lists of verbs must agree: a test holds every non-engine
/// roster verb to an arm here.
pub fn named_sources(tool: &str, args: &Value) -> Option<Vec<Named>> {
    let text = |key: &str| args.get(key).and_then(Value::as_str).map(str::to_string);
    let document = || text("source").map(Named::Document).into_iter().collect();
    let with_reference = |key: &str| {
        let mut named: Vec<Named> = document();
        if let (Some(document), Some(name)) = (text("source"), text(key)) {
            named.push(Named::Reference { document, name });
        }
        named
    };
    Some(match tool {
        names::DESCRIBE_OPERATORS => Vec::new(),
        names::COPY_INSTRUMENT => ["from", "to"]
            .into_iter()
            .filter_map(|key| text(key).map(Named::Document))
            .collect(),
        names::ADD_INSTRUMENT_RESOURCE | names::SET_INSTRUMENT_RESOURCE => {
            with_reference("resource_source")
        }
        names::DESCRIBE_INSTRUMENT
        | names::VALIDATE_INSTRUMENT
        | names::DESCRIBE_BOUNDARY
        | names::WIRE_COMPATIBILITY
        | names::READ_INSTRUMENT
        | names::READ_INSTRUMENT_TREE
        | names::NEW_INSTRUMENT
        | names::RESTORE_INSTRUMENT
        | names::SET_INSTRUMENT_NAME
        | names::SET_INSTRUMENT_DESCRIPTION
        | names::ADD_INSTRUMENT_NODE
        | names::REMOVE_INSTRUMENT_NODE
        | names::RENAME_INSTRUMENT_NODE
        | names::SET_INSTRUMENT_NODE_DESCRIPTION
        | names::SET_INSTRUMENT_INPUT
        | names::SET_INSTRUMENT_INPUTS_BY_INTENT
        | names::WIRE_INSTRUMENT_INPUT
        | names::UNWIRE_INSTRUMENT_INPUT
        | names::SET_INSTRUMENT_CONSTANT
        | names::REMOVE_INSTRUMENT_CONSTANT
        | names::ADD_INSTRUMENT_INTERFACE_INPUT
        | names::ADD_INSTRUMENT_INTERFACE_OUTPUT
        | names::REMOVE_INSTRUMENT_INTERFACE_INPUT
        | names::REMOVE_INSTRUMENT_INTERFACE_OUTPUT
        | names::SET_INSTRUMENT_INTERFACE_INPUT_META
        | names::SET_INSTRUMENT_INTERFACE_OUTPUT_META
        | names::CLEAR_INSTRUMENT_INTERFACE_META
        | names::WIRE_INSTRUMENT_INTERFACE_OUTPUT
        | names::REMOVE_INSTRUMENT_RESOURCE
        | names::SET_INSTRUMENT_NODE_RESOURCE
        | names::SET_INSTRUMENT_LAYOUT => document(),
        _ => return None,
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
    call_scoped(tool, args, store, None)
}

/// The documents a call acts on, as the caller spelled them, and where the first of them, the one
/// whose references a load resolves, lives as the store resolves it.
#[derive(Debug, Clone, Copy)]
pub struct Scope<'a> {
    pub documents: &'a [String],
    pub origin: &'a str,
}

/// [`call`] over a store shared by many documents, scoped to the one `scope` names, so that
/// document's own references resolve beside it rather than from wherever the store's base is.
pub fn call_scoped<R: Resources>(
    tool: &str,
    args: Value,
    store: &DocumentHistory<R>,
    scope: Option<Scope<'_>>,
) -> Result<Called, Refusal> {
    let history: &dyn Resources = store;
    let scoped = scope.map(|scope| ScopedTo {
        store: history,
        documents: scope.documents,
        origin: scope.origin,
    });
    let res: &dyn Resources = match &scoped {
        Some(scoped) => scoped,
        None => history,
    };
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
            run::<DescribeInstrument, _>(args, read, |a| describe_instrument(a, &Undecoded(res)))
        }
        names::VALIDATE_INSTRUMENT => {
            run::<ValidateInstrument, _>(args, read, |a| validate_instrument(a, res))
        }
        names::DESCRIBE_BOUNDARY => {
            run::<DescribeBoundary, _>(args, read, |a| describe_boundary(a, &Undecoded(res)))
        }
        names::WIRE_COMPATIBILITY => {
            run::<WireCompatibility, _>(args, read, |a| wire_compatibility(a, res))
        }
        names::READ_INSTRUMENT => run::<ReadInstrument, _>(args, read, |a| read_instrument(a, res)),
        names::READ_INSTRUMENT_TREE => {
            run::<ReadInstrumentTree, _>(args, read, |a| read_instrument_tree(a, res))
        }

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
            |a| restore_instrument_through(a, store, res),
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
        | names::GET_ENGINE_METERS
        | names::OPEN_INSTRUMENT
        | names::LIST_INSTRUMENTS => Err(Refusal::new(format!(
            "`{tool}` is an engine verb, not served by call"
        ))),
        other => Err(Refusal::new(format!("unknown tool `{other}`"))),
    }
}

#[cfg(test)]
mod tests {
    use alloc::collections::BTreeMap;
    use alloc::string::ToString;
    use alloc::vec;
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
    fn every_non_engine_contract_has_a_named_sources_arm() {
        for contract in CONTRACTS.iter().filter(|c| c.kind != ContractKind::Engine) {
            assert!(
                named_sources(contract.name, &json!({})).is_some(),
                "`{}` has no named_sources arm",
                contract.name
            );
        }
        assert_eq!(named_sources(names::SWAP_INSTRUMENT, &json!({})), None);
    }

    #[test]
    fn named_sources_lists_files_and_not_addresses_ports_or_hashes() {
        let doc = |s: &str| Named::Document(s.to_string());
        let args = json!({"source": "a.json", "from": "/osc", "to": "/osc2"});
        assert_eq!(
            named_sources(names::RENAME_INSTRUMENT_NODE, &args),
            Some(vec![doc("a.json")])
        );
        assert_eq!(
            named_sources(
                names::RESTORE_INSTRUMENT,
                &json!({"source": "a.json", "to": "00c0ffee"})
            ),
            Some(vec![doc("a.json")])
        );
        assert_eq!(
            named_sources(names::COPY_INSTRUMENT, &json!({"from": "a", "to": "b"})),
            Some(vec![doc("a"), doc("b")])
        );
        assert_eq!(
            named_sources(
                names::ADD_INSTRUMENT_RESOURCE,
                &json!({"source": "a.json", "id": "k", "resource_source": "k.wav"})
            ),
            Some(vec![
                doc("a.json"),
                Named::Reference {
                    document: "a.json".into(),
                    name: "k.wav".into()
                }
            ])
        );
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
