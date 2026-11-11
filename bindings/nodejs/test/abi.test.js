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
  // GH #69: the failure carries the structured code; the message names
  // the offending engine (was a generic "tts_create failed" wrapper).
  try {
    new TtsClient({ engineId: "no-such-engine" });
    assert.fail("must throw");
  } catch (err) {
    assert.match(String(err.message), /no-such-engine/);
  }
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

// --- GH #68/#69 parity surface -------------------------------------------

test("engine setups cover every listed engine", () => {
  const setups = TtsClient.engineSetups();
  const listed = new Set(TtsClient.listEngines().map((e) => e.id));
  const schema = new Set(setups.map((s) => s.id));
  assert.equal(listed.size, schema.size, "schema and engine list agree");
  for (const id of listed) assert.ok(schema.has(id), `schema covers ${id}`);
});

test("azure setup renders key + region with typed kinds", () => {
  const azure = TtsClient.engineSetup("azure");
  assert.ok(azure, "azure compiled in under cloud");
  const key = azure.fields.find((f) => f.key === "subscriptionKey");
  assert.equal(key.kind, "secret");
  assert.equal(key.required, true);
  const region = azure.fields.find((f) => f.key === "region");
  assert.equal(region.kind, "choice");
  assert.ok(region.choices.length > 0);
  assert.equal(azure.capabilities.supportsStreaming, true);
});

test("google either/or group is exposed", () => {
  const google = TtsClient.engineSetup("google");
  assert.ok(google, "google compiled in");
  const group = google.requiredAny.find((g) => g.includes("apiKey") && g.includes("keyFilePath"));
  assert.ok(group, "apiKey OR keyFilePath group present");
});

test("create failure throws TtsError with EngineNotAvailable", () => {
  try {
    new TtsClient({ engineId: "definitely-not-an-engine" });
    assert.fail("must throw");
  } catch (err) {
    assert.equal(err.name, "TtsError");
    assert.equal(err.code, 7); // EngineNotAvailable
    assert.equal(err.codeName, "EngineNotAvailable"); // GH #73 review: mapped from native "name"
    assert.match(String(err.message), /definitely-not-an-engine/);
    assert.ok(err.displayMessage, "display hint present");
  }
});

test("missing configuration carries the field key (checkCredentials)", () => {
  const c = new TtsClient({ engineId: "azure", credentials: {} });
  const result = c.checkCredentials();
  assert.equal(result.status, "error");
  assert.equal(result.error.code, 1); // MissingConfiguration
  assert.equal(result.error.fieldKey, "subscriptionKey");
  assert.equal(result.error.engineId, "azure");
  c.close();
});

test("static catalog checks answer unverified (deterministic offline)", () => {
  const c = makeClient(); // openai
  assert.deepEqual(c.checkCredentials(), { status: "unverified" });
  c.close();
});

test("synthesis failure yields a stable code and redacted detail", () => {
  const c = new TtsClient({
    engineId: "openai",
    credentials: {
      apiKey: "sk-abcdef1234567890abcdef",
      synthUrl: "http://127.0.0.1:1/v1/audio/speech", // closed port: deterministic
    },
  });
  try {
    c.synthToBytes("probe");
    assert.fail("must throw");
  } catch (err) {
    assert.equal(err.name, "TtsError");
    assert.equal(err.code, 4); // NetworkError
    assert.ok(!String(err.message).includes("abcdef1234567890"), "secret redacted");
  }
  assert.equal(c.lastErrorCode(), 4);
  assert.ok(c.lastErrorDetail(), "detail readable after the failure");
  c.close();
});
