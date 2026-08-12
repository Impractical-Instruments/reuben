//! A document naming one operator type repeatedly shares the registry's one `Descriptor` rather
//! than copying it. The claim is about `instantiate`/`Registry`, but only the loader can express
//! "a document naming one type three times", so the test lives here.

use std::sync::Arc;

use reuben_core::Registry;

#[test]
fn a_document_naming_one_type_three_times_shares_one_descriptor() {
    let registry = Registry::builtin();
    let json = r#"{"instrument":"t","nodes":[
        {"type":"oscillator","address":"/a"},
        {"type":"oscillator","address":"/b"},
        {"type":"oscillator","address":"/c"}]}"#;
    let graph = reuben_document::load(json, &registry).expect("three oscillators load");

    let registered = &registry
        .get("oscillator")
        .expect("builtin oscillator")
        .descriptor;
    for address in ["/a", "/b", "/c"] {
        let key = graph.find(address).expect("node built");
        assert!(
            Arc::ptr_eq(&graph.nodes[key].descriptor, registered),
            "{address} holds the registry's descriptor, not a copy of it"
        );
    }
}
