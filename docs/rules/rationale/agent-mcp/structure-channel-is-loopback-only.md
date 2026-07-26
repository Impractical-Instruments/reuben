# Why: The structure channel binds loopback only, and its one default address lives beside the wire types both ends serialize.

[Rule](../../agent-mcp.md#structure-channel-is-loopback-only)

The engine listens on two sockets with very different authority, and conflating their exposure would
be the system's worst security mistake. OSC-the-wire is a **foreign edge**: it carries control —
turn this knob, play this note — from anything on the network a performer chooses to point at it,
and binding it broadly is the whole point of a networked instrument. The structure channel carries
**structure edits**: swap the document, rewire the graph, replace what the instrument *is*. Reaching
it is equivalent to arbitrary control over what the machine plays, and it is the authoring door, used
by a sidecar running on the same machine as the engine it serves.

Nothing about that door's job requires network reach, so it binds `127.0.0.1` only. This is a
default that must not quietly become configurable in the direction of exposure: "let me author from
my laptop against the studio machine" is a reasonable-sounding request whose implementation is an
unauthenticated remote graph-edit socket. The right answer to it is a tunnel the operator sets up
deliberately, not a config key that makes the insecure case one edit away.

The second half of the rule is about drift rather than security. The server that binds and the
client that dials are in different crates, and an address duplicated across them is an address that
can disagree — a failure that presents as "the sidecar cannot reach the engine" with two plausible
and equally wrong explanations. So the one literal is shared, and shared *next to the wire types
both ends serialize*, because that is the place both ends must already agree, and agreement kept in
one place is agreement that cannot be half-updated. Sharing a constant is justified by two ends
needing to agree, and it ends when they no longer do.

Only the *address of the address* has moved since: it is not a constant two doors both reach into
the engine for, but one [the window owns and both doors are handed](structure-channel-envelope.md).
The rule is unchanged by that, which is the point — a relocation that leaves a rule true is not an
overturn.

Decided in: issue #639 — settled directly, no ADR.
