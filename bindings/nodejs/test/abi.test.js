// ABI conformance tests for the Node.js binding — mirrors bindings/c
// (the C acceptance harness) and tests/ffi_conformance.rs.
//
// Requires the shared library to be built:
//   cargo build --no-default-features --features system,cloud
// (or set TTS_WRAPPER_LIB to an explicit path).

"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const { TtsClient, loadLibrary } = require("../src/index.js");

function makeClient() {
  // openai constructs offline; synthesis fails deterministically with a
  // dummy key — the exact contract these tests assert.
  return new TtsClient({
    engineId: "openai",
    credentials: { apiKey: "dummy-key-for-node-tests" },
  });
}

test("engine enumeration", () => {
  const count = TtsClient.engineCount();
  assert.ok(count > 0, "engine count must be positive");

  const engines = TtsClient.listEngines();
  assert.equal(engines.length, count, "listEngines matches engineCount");
  for (const e of engines) {
    assert.ok(typeof e.id === "string" && e.id.length > 0, "engine id");
    assert.ok(typeof e.name === "string" && e.name.length > 0, "engine name");
    assert.equal(typeof e.needsCredentials, "boolean", "needsCredentials");
    assert.ok(Array.isArray(e.credentialKeys), "credentialKeys array");
  }
  assert.ok(engines.some((e) => e.id === "openai"), "openai is compiled in");
});

test("create / lifecycle / double close", () => {
  const c = makeClient();
  c.close();
  c.close(); // idempotent
  assert.throws(() => c.speak("x"), /closed/);
});

test("create failure surfaces the global error", () => {
  assert.throws(() => new TtsClient({ engineId: "no-such-engine" }), /tts_create/);
});

test("many clients live simultaneously", () => {
  const clients = Array.from({ length: 8 }, makeClient);
  for (const c of clients) assert.ok(c.getVoices() !== undefined);
  for (const c of clients) c.close();
});

test("setters accept typical values", () => {
  const c = makeClient();
  c.setVoice("alloy");
  c.setVoice("");
  c.setRate(1.5);
  c.setPitch(0.8);
  c.setVolume(0.9);
  c.stop();
  c.pause();
  c.resume();
  c.close();
});

test("getVoices returns an array (empty offline is fine)", () => {
  const c = makeClient();
  const voices = c.getVoices();
  assert.ok(Array.isArray(voices));
  for (const v of voices) {
    assert.ok(typeof v.id === "string", "voice id is a string");
  }
  c.close();
});

test("speak failures surface as throws or error events (dummy key)", () => {
  const c = makeClient();
  const errors = [];
  c.on("error", (msg) => errors.push(msg));

  // With a dummy key every path fails, offline (validation) or online
  // (401) — either as a throw or via the error event. Never silently.
  const outcomes = [];
  outcomes.push(tryCall(() => c.speakSync("hello node")));
  outcomes.push(tryCall(() => c.synthToBytes("hello node")));
  c.close();

  const failed = outcomes.some((o) => o === "threw") || errors.length > 0;
  assert.ok(failed, "dummy-key synthesis must fail in some observable way");
});

function tryCall(fn) {
  try {
    fn();
    return "returned";
  } catch {
    return "threw";
  }
}

test("boundary / mark / viseme callback registration does not throw", () => {
  // Registration-only: synthesis outcome depends on network reachability,
  // so this test asserts the trampolines wire up (and stay silent when
  // nothing fires), not delivery. Delivery is covered by the engine
  // suites and live tests.
  const c = makeClient();
  let boundaries = 0;
  let marks = 0;
  let visemes = 0;
  c.on("boundary", (ev) => {
    boundaries++;
    assert.equal(typeof ev.word, "string");
    assert.equal(typeof ev.estimated, "boolean");
  });
  c.on("mark", () => marks++);
  c.on("viseme", () => visemes++);
  c.close();
  assert.equal(boundaries, 0);
  assert.equal(marks, 0);
  assert.equal(visemes, 0);
});

test("loadLibrary with explicit path rejects a bad path clearly", () => {
  assert.throws(() => loadLibrary("/nonexistent/lib.so"), /not found|Cannot open/);
});
