# Why: The licence boundary is the repo boundary: this repo is BSD-3-Clause, the private product repo is AGPL-3.0, and no file is dual-licensed.

[Rule](../../web-product-process.md#license-boundary)

Making the licence boundary the *repo* boundary means "which licence governs this code" is answered
by "which repo is it in" — no file is dual-headed, no per-file header audit, no mixed-licence
directory. This repo is **BSD-3-Clause** (the permissive SDK); the private product repo is
**AGPL-3.0**, copyleft over both the served client and the network relay, with a dual-licence option
preserved. Copyleft belongs on the product side because that is where the served-client / network-use
surface is; the SDK stays permissive so anything can embed the engine.

The licence split and the [SDK/product split](sdk-product-split.md) coincide but are not the same
decision — see that rationale for why the shell moved.

Distilled from: ADR-0056
