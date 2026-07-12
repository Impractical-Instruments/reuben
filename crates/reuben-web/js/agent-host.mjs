// agent-host.mjs — the streaming conversation loop, browser side (issue #354, ADR-0054 §2).
//
// The reshape turn is a RELAYED, client-side-execution loop (ADR-0054 §2): the model conversation
// runs through the hosted proxy, but the eight tools EXECUTE in the browser against the live
// worklet (ADR-0052 §2). This module owns the engine-facing half:
//
//   user turn in
//     → transport(messages) streams the model's tokens back (SSE relayed through the proxy)
//     → tokens append to the turn's plan IN PLACE (spec §4.2 — streaming is LOAD-BEARING)
//     → a `tool_use` is dispatched into the M0 tool layer's eight tools (js/tools.mjs)
//     → the tool_result is fed back as the next user turn
//     → repeat until the assistant turn resolves (stop_reason: end_turn)
//
// The proxy owns the model-facing half (system prompt, tool-schema declaration, model id, SSE
// passthrough — ADR-0054 §2/§3), so this module never sees an API key and never declares tools:
// it POSTs `{messages}` and consumes the streamed events. That makes the whole loop testable
// against a deterministic MOCK transport with no network and no key (the merge-gating path).
//
// Error discipline (ADR-0048 §3, and this ticket): a tool that throws "could not do its job"
// surfaces to the AGENT as a tool_result with is_error, which the model sees and self-repairs
// from — NOT to the user (failure UX is M2). A `{ok:false}` validate/swap is the tool WORKING and
// flows back as an ordinary result.

import { userTurn, assistantTurn, stubTranscript, RESTART_HONESTY_LINE } from "./agent-turn.mjs";

/**
 * Parse a proxied SSE Response body (ADR-0054 §2's straight-through stream) into the Anthropic
 * message-stream events the loop consumes. Yields the parsed `data:` payloads (each carries its
 * own `type`); `event:` lines and keepalives are ignored. This is the SEAM the real proxy transport
 * uses; the mock transport yields events directly and never touches SSE.
 *
 * @param {Response} response - a fetch Response whose body is an SSE stream.
 * @returns {AsyncGenerator<object>} parsed stream events.
 */
export async function* sseEvents(response) {
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });
    let sep;
    // Events are separated by a blank line; a `data:` line inside carries one JSON event.
    while ((sep = buffer.indexOf("\n\n")) !== -1) {
      const frame = buffer.slice(0, sep);
      buffer = buffer.slice(sep + 2);
      for (const line of frame.split("\n")) {
        const trimmed = line.replace(/^\s+/, "");
        if (!trimmed.startsWith("data:")) continue;
        const data = trimmed.slice(5).trim();
        if (!data || data === "[DONE]") continue;
        try {
          yield JSON.parse(data);
        } catch {
          // A malformed/keepalive frame is not a stream event — skip it.
        }
      }
    }
  }
}

/**
 * Build the production transport: POST the conversation to the proxy relay and stream the SSE
 * passthrough back. The proxy adds the system prompt, tool schemas, and model id (ADR-0054 §2), so
 * the browser sends only `{messages}`. A non-2xx proxy response (e.g. the key is absent → 5xx,
 * ADR-0054 §5) throws here; the sensory failure copy is M2's job (§5.3).
 *
 * @param {string} endpoint - the proxy URL (e.g. "/api/chat").
 * @param {object} [opts]
 * @param {typeof fetch} [opts.fetchImpl] - injectable for tests; defaults to global fetch.
 * @returns {(messages: object[]) => Promise<AsyncIterable<object>>}
 */
export function proxyTransport(endpoint, { fetchImpl = fetch } = {}) {
  return async (messages) => {
    const response = await fetchImpl(endpoint, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ messages }),
    });
    if (!response.ok) {
      // Host/transport failure class (ADR-0054 §6): collapse to a throw here; the loop's caller
      // maps it to §5.3's terminal-failure shape in M2. Never expose the host reason to the user.
      throw new Error(`chat proxy responded ${response.status}`);
    }
    return sseEvents(response);
  };
}

/**
 * Drain one model turn's event stream into an assistant `content` array, appending each streamed
 * text token to the turn's plan (spec §4.2). Accumulates tool_use inputs from either an inline
 * `content_block.input` or streamed `input_json_delta` fragments (the real wire form).
 *
 * @returns {Promise<{ content: object[], stopReason: string|null }>}
 */
async function consumeStream(stream, builder, transcript) {
  const blocks = []; // indexed by the stream's content-block index
  let stopReason = null;

  for await (const ev of stream) {
    switch (ev.type) {
      case "content_block_start": {
        const cb = ev.content_block || {};
        if (cb.type === "tool_use") {
          blocks[ev.index] = {
            type: "tool_use",
            id: cb.id,
            name: cb.name,
            input: cb.input, // may be undefined; filled from input_json_delta below
            _json: "",
          };
        } else {
          blocks[ev.index] = { type: cb.type || "text", text: cb.text || "" };
        }
        break;
      }
      case "content_block_delta": {
        const b = blocks[ev.index];
        if (!b) break;
        const d = ev.delta || {};
        if (d.type === "text_delta") {
          b.text += d.text;
          builder.appendPlan(d.text); // §4.2: plan grows in place, token by token
          transcript.onPlanDelta(builder.turn, d.text);
        } else if (d.type === "input_json_delta") {
          b._json += d.partial_json || "";
        }
        break;
      }
      case "content_block_stop": {
        const b = blocks[ev.index];
        if (b && b.type === "tool_use" && b.input === undefined) {
          b.input = b._json ? JSON.parse(b._json) : {};
        }
        break;
      }
      case "message_delta":
        if (ev.delta && ev.delta.stop_reason) stopReason = ev.delta.stop_reason;
        break;
      // message_start / message_stop / unknown: nothing to accumulate.
      default:
        break;
    }
  }

  const content = blocks
    .filter(Boolean)
    .map((b) => {
      if (b.type === "tool_use") return { type: "tool_use", id: b.id, name: b.name, input: b.input };
      if (b.type === "text") return { type: "text", text: b.text };
      return b;
    })
    // The Messages API rejects an empty text block; a text block that opened but never streamed a
    // delta (text === "") must not go back upstream in the assistant turn.
    .filter((b) => !(b.type === "text" && b.text === ""));
  return { content, stopReason };
}

/**
 * Create the agent host over an in-page tool layer (js/tools.mjs `createToolLayer`) and a transport
 * (the proxy in production, a mock in tests). Maintains the Anthropic message history and drives
 * the streamed reshape loop, emitting turn envelopes (js/agent-turn.mjs) to the transcript sink.
 *
 * @param {object} deps
 * @param {Record<string, Function>} deps.toolLayer - the eight contract tools, keyed by name.
 * @param {(messages: object[]) => Promise<AsyncIterable<object>>} deps.transport - streams events.
 * @param {ReturnType<typeof stubTranscript>} [deps.transcript] - the transcript sink (stub proves it).
 * @param {number} [deps.maxRounds] - safety bound on tool round-trips per turn (network-bound loop).
 * @returns {{ send: (text: string) => Promise<import("./agent-turn.mjs").AgentTurn>, messages: object[] }}
 */
export function createAgentHost({ toolLayer, transport, transcript = stubTranscript(), maxRounds = 8 }) {
  const messages = [];
  // Session-scoped gate for spec §6.4's first-run re-strike line: one AgentHost instance IS one
  // session, so a plain closure flag is the whole mechanism — flips true the first time a swap
  // genuinely restarts already-playing sound (js/tools.mjs `restarted`), stays wordless after.
  let restartHonestyGiven = false;

  /**
   * Run one user turn to resolution: stream, dispatch tools, feed results, repeat, resolve.
   * @param {string} text - the user's plain-language turn.
   * @returns {Promise<import("./agent-turn.mjs").AgentTurn>} the resolved assistant turn envelope.
   */
  async function send(text) {
    const u = userTurn(text);
    transcript.onUserTurn(u);
    messages.push({ role: "user", content: text });

    const builder = assistantTurn();
    transcript.onTurnStart(builder.turn);

    let truncated = true; // flipped false when the turn resolves within the round budget
    for (let round = 0; round < maxRounds; round += 1) {
      const stream = await transport(messages);
      const { content, stopReason } = await consumeStream(stream, builder, transcript);
      messages.push({ role: "assistant", content });

      const toolUses = content.filter((b) => b.type === "tool_use");
      if (stopReason !== "tool_use" || toolUses.length === 0) {
        truncated = false;
        break; // resolved
      }

      // Dispatch every tool_use into the in-page layer and collect ALL results in ONE user turn
      // (the tool-result protocol requires one result per tool_use id, together).
      const toolResults = [];
      for (const tu of toolUses) {
        const inv = { id: tu.id, name: tu.name, input: tu.input, isError: false };
        const fn = toolLayer[tu.name];
        try {
          if (typeof fn !== "function") throw new Error(`unknown tool: ${tu.name}`);
          const result = await fn(tu.input); // swap is async; others sync — await handles both
          inv.result = result;
          // A successful swap carries the structural diff the card renders (spec §4.6).
          if (tu.name === "swap" && result && result.diff) builder.setDiff(result.diff);
          // F's content for D's reserved slot (spec §6.4): the FIRST genuine restart of
          // already-playing sound this session gets one light framing line; every one after is
          // wordless. `restarted` (js/tools.mjs) is false for a first install into silence
          // (nothing to be honest about, §6.4's consequence), so that case never trips this.
          if (tu.name === "swap" && result && result.restarted && !restartHonestyGiven) {
            builder.setRestartHonesty(RESTART_HONESTY_LINE);
            restartHonestyGiven = true;
          }
          toolResults.push({
            type: "tool_result",
            tool_use_id: tu.id,
            content: JSON.stringify(result ?? null),
          });
        } catch (err) {
          // Surfaces to the AGENT, not the user (ADR-0048 §3, this ticket): the model sees the
          // error as a tool_result and self-repairs; no user-facing failure copy here (M2).
          inv.isError = true;
          inv.result = String((err && err.message) || err);
          toolResults.push({
            type: "tool_result",
            tool_use_id: tu.id,
            content: inv.result,
            is_error: true,
          });
        }
        builder.recordTool(inv);
      }
      messages.push({ role: "user", content: toolResults });
    }

    if (truncated) {
      // Hit the round budget with tools still pending (network-bound loop safety bound). We resolve
      // the turn so the sound isn't left broken; `failed` stays reserved for M2's §5.3 taxonomy.
      console.warn(
        `[reuben-agent-host] turn ${builder.turn.id} hit maxRounds (${maxRounds}) with tools still ` +
          "pending — resolving anyway (terminal-failure UX is M2, §5.3).",
      );
    }

    const resolved = builder.resolve();
    transcript.onTurnResolved(resolved);
    return resolved;
  }

  return {
    send,
    // A shallow copy so a caller can inspect the history without mutating the loop's internal state.
    get messages() {
      return messages.slice();
    },
  };
}
