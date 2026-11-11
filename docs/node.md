# Node.js usage

npm package: [`@aactools/tts-wrapper`](https://www.npmjs.com/package/@aactools/tts-wrapper)
· Source: [`bindings/nodejs/`](../bindings/nodejs/) (N-API preload of the
shared library, then the same flat C ABI).

```js
const { TtsClient } = require("@aactools/tts-wrapper");

const client = new TtsClient({ engineId: "openai", credentials: { apiKey: "your-key" } });

client.on("audio", ({ chunk }) => play(chunk));
client.on("boundary", ({ word, startSec, endSec, estimated }) =>
  console.log(`${word}: ${startSec}-${endSec} ${estimated ? "estimated" : "measured"}`));

client.setVoice("alloy");
client.setRate(1.1);
client.speakSync("Hello world");
client.stop();
client.close();
```

## Loading the native library

Set `TTS_WRAPPER_LIB` to an explicit path, or place
`rust_tts_wrapper.node`/the shared library where the loader finds it:

```bash
cargo build --release --no-default-features --features system,cloud
TTS_WRAPPER_LIB=target/release/librust_tts_wrapper.so node demo.js
```

The `js/` directory additionally hosts the unified JavaScript/Node
package (`speak()` across floravox + cloud engines); see
[`js/`](../js/) for its README.

## Engine selection & credentials

```js
const engines = TtsClient.listEngines();        // ids compiled into this build
const voices  = client.getVoices();             // per-engine voice list

const azure = new TtsClient({
  engineId: "azure",
  credentials: { subscriptionKey: "...", region: "eastus" },
});
```

Credentials are plain objects serialised to the JSON the Rust factory
parses (canonical keys — see the [engines doc](engines.md)).

## Errors, credentials and the setup schema (GH #68/#69/#73)

```js
// 1) Structured errors: TtsError carries the stable code, the
//    offending field key and a display-ready hint (messages are
//    secret-redacted by the native layer).
try {
  client.synthToBytes("hello");
} catch (err) {
  err.code;            // 4 (NetworkError)
  err.fieldKey;        // "subscriptionKey" for missing-config failures
  err.displayMessage;  // safe hint
}
client.lastErrorCode();    // stable u32 after any failure
client.lastErrorDetail();  // {code, codeName, message, displayMessage, engineId, fieldKey}
                           // (codeName is mapped from the native "name" key)

// 2) Tri-state credential check: static catalogs answer "unverified".
const check = client.checkCredentials();
// { status: "verified" | "rejected" | "unverified" }
// { status: "error", error: TtsErrorDetail }  — e.g. missing field

// 3) Typed engine setup schema — render settings UIs without a
//    provider switch (kinds: secret / filePath / directoryPath / url /
//    choice / number / text; either/or groups; capabilities).
const setups = TtsClient.engineSetups();
const azure = TtsClient.engineSetup("azure");
// azure.fields: subscriptionKey (secret, required), region (choice, choices[…])
```

Unknown field kinds from newer native builds degrade to `"text"`.
