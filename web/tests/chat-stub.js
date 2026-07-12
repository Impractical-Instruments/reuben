// tests/chat-stub.js — a deterministic in-page STUB for the web-chat agent transport (issue #397
// verification). It installs `window.__REUBEN_CHAT_TRANSPORT__` — the test seam main.js hands to
// createChatHost — so a Playwright spec drives the REAL onReshapeSubmit / generate loop (chat-host →
// tools → agent-turn → change-card) with NO key and NO network, scripting the model's streamed plan
// and its tool-use per turn. This is the browser mirror of crates/reuben-web/js/live-eval.mjs's
// plumbing, minus the live model — the merge-gating half (live-model behavior is the self-gated
// live smoke's job).
//
// The stub emits the same Anthropic message-stream events agent-host.mjs's consumeStream consumes.
// A turn is two rounds when it uses a tool: round 1 streams the plan + a tool_use (stop_reason
// "tool_use"); the host executes the tool against the LIVE worklet and feeds the result back; round
// 2 (the stub sees a tool_result) streams a short close (stop_reason "end_turn"). A no-tool turn is
// one round. Per-turn scripting is set from the test via `setTurn(page, {...})`.

/**
 * Install the stub transport + its control object (`window.__chatStub`) before the app boots.
 * @param {import('@playwright/test').Page} page
 */
export async function installChatStub(page) {
  await page.addInitScript(() => {
    const wordChunks = (t) => t.match(/\S+\s*/g) || [t];
    async function* textBlock(text) {
      // A text block that streams token-ish chunks — so a spec can prove the plan renders
      // INCREMENTALLY (spec §4.2), not as one final blob.
      yield { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } };
      for (const c of wordChunks(text)) {
        yield { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: c } };
      }
      yield { type: "content_block_stop", index: 0 };
    }

    // Per-turn script, mutated by the test via page.evaluate before each submit.
    //   plan   — the sensory line streamed into the card.
    //   action — {type:'none'} | {type:'send', address, value} | {type:'swap', document}.
    //   fail   — throw from the transport (a host/transport drop → §5.3 terminal).
    //   hold   — block the transport until cleared (so the in-flight stripe is observable).
    const state = { plan: "Shaping that now.", action: { type: "none" }, fail: false, hold: false };
    window.__chatStub = state;

    window.__REUBEN_CHAT_TRANSPORT__ = async function* transport(messages) {
      while (window.__chatStub.hold) await new Promise((r) => setTimeout(r, 20));
      if (window.__chatStub.fail) throw new Error("stub transport down");

      const last = messages[messages.length - 1];
      const isToolResult = last && last.role === "user" && Array.isArray(last.content);
      if (isToolResult) {
        // Round 2: the tool ran; close the turn.
        yield* textBlock("There you go.");
        yield { type: "message_delta", delta: { stop_reason: "end_turn" } };
        return;
      }

      // Round 1: stream the plan, then optionally a tool_use.
      yield* textBlock(window.__chatStub.plan);
      const a = window.__chatStub.action || { type: "none" };
      if (a.type === "send" || a.type === "swap") {
        const input =
          a.type === "send"
            ? { messages: [{ address: a.address, args: [a.value] }] }
            : { document: a.document };
        yield {
          type: "content_block_start",
          index: 1,
          content_block: { type: "tool_use", id: "tool-1", name: a.type, input },
        };
        yield { type: "content_block_stop", index: 1 };
        yield { type: "message_delta", delta: { stop_reason: "tool_use" } };
        return;
      }
      // No tool: a §5.2 no-change turn (the plan IS the reply).
      yield { type: "message_delta", delta: { stop_reason: "end_turn" } };
    };
  });
}

/** Configure the next turn(s). Merges into the in-page control object. */
export function setTurn(page, cfg) {
  return page.evaluate((c) => Object.assign(window.__chatStub, c), cfg);
}
