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
