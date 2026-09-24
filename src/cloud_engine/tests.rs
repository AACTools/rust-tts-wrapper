use super::*;

#[cfg(test)]
mod tests {
    use super::*;
    // ===== streaming delivery =================================================

    /// Build a silent MP3 stream: N MPEG-1 Layer III 44.1 kHz mono frames
    /// (128 kbps → 417 bytes each) with zeroed payloads. Zeroed granule
    /// data decodes deterministically to silence, which makes it usable as
    /// a byte-stable fixture (symphonia is compiled mp3-only, so WAV
    /// fixtures can't be decoded by the reference path).
    pub(crate) fn make_silent_mp3(frames: usize) -> Vec<u8> {
        let frame_len = 417; // 144 * 128000 / 44100, no padding
        let mut mp3 = Vec::with_capacity(frames * frame_len);
        for _ in 0..frames {
            // FF FB 90 C0: sync, MPEG1 L3, 128kbps, 44.1kHz, mono
            mp3.extend_from_slice(&[0xFF, 0xFB, 0x90, 0xC0]);
            mp3.extend(std::iter::repeat_n(0u8, frame_len - 4));
        }
        mp3
    }

    /// A reader that hands out data in small dribbles with tiny pauses,
    /// simulating a slow network body.
    struct DribbleReader {
        data: Vec<u8>,
        pos: usize,
        piece: usize,
    }
    impl std::io::Read for DribbleReader {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            if self.pos >= self.data.len() {
                return Ok(0);
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
            let n = out.len().min(self.piece).min(self.data.len() - self.pos);
            out[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    #[test]
    pub(crate) fn streaming_decode_matches_buffered_decode() {
        let mp3 = make_silent_mp3(20);
        // Reference: whole-buffer mono decode.
        let expected = decode_mp3_to_pcm16_mono(&mp3);
        assert!(!expected.is_empty(), "reference decode produced audio");

        let reader = DribbleReader {
            data: mp3,
            pos: 0,
            piece: 97, // awkward non-aligned size on purpose
        };
        let mut collected: Vec<u8> = Vec::new();
        let mut deliveries = 0usize;
        let total = stream_body_to_on_audio(reader, false, 24_000, None, &mut |ev| {
            if let StreamEvt::Audio(chunk) = ev {
                collected.extend_from_slice(chunk);
                deliveries += 1;
            }
        })
        .expect("stream");
        assert_eq!(total, collected.len());
        assert_eq!(collected, expected, "byte-identical to buffered decode");
        assert!(deliveries > 1, "expected incremental delivery");
    }

    #[test]
    pub(crate) fn streaming_pcm_passthrough_preserves_bytes() {
        let pcm: Vec<u8> = (0..10_000u16).flat_map(u16::to_le_bytes).collect();
        let reader = DribbleReader {
            data: pcm.clone(),
            pos: 0,
            piece: 4096,
        };
        let mut collected: Vec<u8> = Vec::new();
        let total = stream_body_to_on_audio(reader, true, 24_000, None, &mut |ev| {
            if let StreamEvt::Audio(chunk) = ev {
                collected.extend_from_slice(chunk);
            }
        })
        .expect("stream");
        assert_eq!(total, pcm.len());
        assert_eq!(collected, pcm);
    }

    #[test]
    pub(crate) fn estimated_boundaries_fire_progressively_during_streaming() {
        // Long-ish silent MP3 (each frame ≈ 26 ms at 44.1 kHz) dribbled
        // slowly through the decode path: every estimate must fire, and
        // the flush path must cover audio shorter than the estimates.
        let mp3 = make_silent_mp3(80); // ≈ 2.1 s of audio
        let plan = EstimatePlan::build("one two three four five six seven");
        let expected = plan.len();
        assert!(expected > 0);

        let reader = DribbleReader {
            data: mp3,
            pos: 0,
            piece: 97,
        };
        let mut audio_chunks = 0usize;
        let mut boundaries: Vec<String> = Vec::new();
        stream_body_to_on_audio(reader, false, 24_000, Some(plan), &mut |ev| match ev {
            StreamEvt::Audio(_) => audio_chunks += 1,
            StreamEvt::Boundary(word, ..) => boundaries.push(word.to_string()),
        })
        .expect("stream");
        assert!(!boundaries.is_empty(), "no boundaries fired");
        // Every estimate eventually fired (flush covers short audio).
        assert_eq!(boundaries.len(), expected);
        assert!(audio_chunks > 1);
    }

    #[test]
    pub(crate) fn estimated_boundaries_interleave_with_audio_events() {
        // Record the exact event sequence; at least one Boundary must
        // appear between two Audio events (i.e. before the stream ended).
        use EventKind::{Audio as AudioEvt, Boundary as BoundaryEvt};

        #[derive(PartialEq, Debug, Clone, Copy)]
        enum EventKind {
            Audio,
            Boundary,
        }

        let mp3 = make_silent_mp3(80);
        let plan = EstimatePlan::build("one two three four five six seven");
        let reader = DribbleReader {
            data: mp3,
            pos: 0,
            piece: 97,
        };
        let mut seq: Vec<EventKind> = Vec::new();
        let mut record = |ev: StreamEvt<'_>| match ev {
            StreamEvt::Audio(..) => seq.push(AudioEvt),
            StreamEvt::Boundary(..) => seq.push(BoundaryEvt),
        };
        stream_body_to_on_audio(reader, false, 24_000, Some(plan), &mut record).expect("stream");
        // Find a Boundary that is followed by at least one more Audio →
        // it fired during streaming, not at the end flush.
        let interleaved = seq
            .iter()
            .enumerate()
            .any(|(i, k)| *k == BoundaryEvt && seq[i + 1..].contains(&AudioEvt));
        assert!(
            interleaved,
            "expected a boundary fired before the final audio chunk; seq = {seq:?}"
        );
    }

    #[test]
    pub(crate) fn estimate_plan_strips_ssml_before_estimating() {
        let plain = EstimatePlan::build("hello world");
        let ssml = EstimatePlan::build("<speak>hello <break time=\"1s\"/> world</speak>");
        assert_eq!(plain.len(), ssml.len());
        let words: Vec<String> = (0..ssml.len())
            .map(|i| ssml.event(i).expect("in range").word.clone())
            .collect();
        assert_eq!(words, vec!["hello", "world"]);
        // Offsets resolved into the stripped text: "world" found at a
        // valid position (not -1).
        assert!(ssml.event(1).expect("in range").char_offset > 0);
    }

    #[test]
    pub(crate) fn streaming_undecodable_body_reports_error() {
        let garbage = b"this is definitely not audio".to_vec();
        let reader = DribbleReader {
            data: garbage,
            pos: 0,
            piece: 4096,
        };
        let result = stream_body_to_on_audio(reader, false, 24_000, None, &mut |_| {});
        assert!(result.is_err(), "garbage body should surface probe failure");
    }

    #[test]
    pub(crate) fn test_build_azure_ssml() {
        let ssml = build_azure_ssml("Hello world", "en-US-AriaNeural", 1.0, 1.0, 1.0);
        assert!(ssml.contains("<speak"));
        assert!(ssml.contains("en-US-AriaNeural"));
        assert!(ssml.contains("Hello world"));
        assert!(!ssml.contains("<prosody"));
    }

    #[test]
    pub(crate) fn test_build_azure_ssml_with_prosody() {
        let ssml = build_azure_ssml("Hello world", "en-US-AriaNeural", 1.5, 0.8, 1.4);
        assert!(ssml.contains("<prosody"));
        // Percentage-based prosody: rate=1.5 → +50%, pitch=0.8 → -10%, volume=1.4 → +40%
        assert!(ssml.contains("rate=\"+50%\""));
        assert!(ssml.contains("pitch=\"-10%\""));
        assert!(ssml.contains("volume=\"+40%\""));
    }

    // ===== normalize_ssml_envelope =====

    #[test]
    pub(crate) fn test_normalize_envelope_fills_missing_attributes() {
        // Exactly what speech-dispatcher's index-marking wrapper sends:
        // a bare <speak>. Azure/Edge accept it but synthesise no audio.
        let result = normalize_ssml_envelope(
            "<speak>Repeat <mark name=\"m1\"/> test</speak>",
            "en-GB-SoniaNeural",
        );
        let expected_prefix =
            format!("<speak version=\"1.0\" xmlns=\"{SSML_XMLNS}\" xml:lang=\"en-GB\">");
        assert!(
            result.starts_with(&expected_prefix),
            "envelope attributes must be added in order: {result}"
        );
        assert!(result.ends_with("Repeat <mark name=\"m1\"/> test</speak>"));
    }

    #[test]
    pub(crate) fn test_normalize_envelope_keeps_present_attributes() {
        let ssml = "<speak version=\"1.1\" xmlns=\"urn:custom\" xml:lang=\"de-DE\">Hallo</speak>";
        assert_eq!(normalize_ssml_envelope(ssml, "en-US-AriaNeural"), ssml);
    }

    #[test]
    pub(crate) fn test_normalize_envelope_fills_only_gaps() {
        let result = normalize_ssml_envelope(
            "<speak xml:lang='fr-FR'>Bonjour</speak>",
            "en-US-AriaNeural",
        );
        assert!(result.contains("xml:lang='fr-FR'"), "existing lang kept");
        assert!(result.contains("version=\"1.0\""), "version added");
        assert!(result.contains("xmlns="), "xmlns added");
        // The added xml:lang must not duplicate the existing one.
        assert_eq!(result.matches("xml:lang").count(), 1);
    }

    #[test]
    pub(crate) fn test_normalize_envelope_leaves_plain_text_and_fragments_alone() {
        assert_eq!(
            normalize_ssml_envelope("Angle < bracket", "en-US-AriaNeural"),
            "Angle < bracket"
        );
        // Envelope-less SSML fragment (no <speak tag) is untouched.
        assert_eq!(
            normalize_ssml_envelope("<prosody rate='slow'>hi</prosody>", "en-US-AriaNeural"),
            "<prosody rate='slow'>hi</prosody>"
        );
        // Unterminated tag — left alone rather than mangled.
        assert_eq!(
            normalize_ssml_envelope("<speak version=", "en-US-AriaNeural"),
            "<speak version="
        );
    }

    #[test]
    pub(crate) fn test_normalize_envelope_preserves_tag_case_and_self_closing() {
        let result = normalize_ssml_envelope("<SPEAK>Hi</SPEAK>", "en-US-AriaNeural");
        assert!(result.starts_with("<SPEAK version="), "tag name case kept");
        assert!(result.ends_with("Hi</SPEAK>"));

        let result = normalize_ssml_envelope("<speak/>", "en-US-AriaNeural");
        let expected =
            format!("<speak version=\"1.0\" xmlns=\"{SSML_XMLNS}\" xml:lang=\"en-US\"/>");
        assert_eq!(result, expected);
    }

    #[test]
    pub(crate) fn test_normalize_envelope_composes_with_voice_injection() {
        // The WS/REST send path normalizes first, then injects <voice>.
        let result = inject_voice_if_missing(
            &normalize_ssml_envelope("<speak>hello</speak>", "en-GB-SoniaNeural"),
            "en-GB-SoniaNeural",
        );
        assert!(result.contains("version=\"1.0\""));
        assert!(result.contains("xml:lang=\"en-GB\""));
        assert!(result.contains("<voice name='en-GB-SoniaNeural'>"));
    }

    #[test]
    pub(crate) fn test_voice_lang_from_voice_name() {
        assert_eq!(voice_lang("en-GB-SoniaNeural"), "en-GB");
        assert_eq!(voice_lang("en-US-AvaMultilingualNeural"), "en-US");
        assert_eq!(voice_lang("alloy"), "en-US"); // not a locale
        assert_eq!(voice_lang(""), "en-US");
    }

    // ===== strip_unsupported_marks =====

    #[test]
    pub(crate) fn test_strip_marks_removes_self_closing_and_paired() {
        // The exact shape speech-dispatcher wraps around pauses.
        assert_eq!(
            strip_unsupported_marks("A <mark name=\"__spd_0\"/> B", false),
            "A  B"
        );
        assert_eq!(
            strip_unsupported_marks("A <mark name='x'></mark> B", false),
            "A  B"
        );
    }

    #[test]
    pub(crate) fn test_strip_marks_leaves_similar_names_and_text_alone() {
        assert_eq!(
            strip_unsupported_marks("<market price='3'>", false),
            "<market price='3'>"
        );
        assert_eq!(strip_unsupported_marks("a < b", false), "a < b");
        assert_eq!(strip_unsupported_marks("<mark", false), "<mark"); // unterminated
        assert_eq!(
            strip_unsupported_marks("no marks here", false),
            "no marks here"
        );
    }

    #[test]
    pub(crate) fn test_strip_marks_full_speechd_document() {
        let ssml = "<speak>Hello <mark name=\"__spd_0\"/> world</speak>";
        let stripped = strip_unsupported_marks(ssml, false);
        assert!(!stripped.contains("mark"));
        assert_eq!(stripped, "<speak>Hello  world</speak>");
    }

    #[test]
    pub(crate) fn test_bookmark_kept_for_azure_stripped_for_edge() {
        // Azure documents <bookmark mark=…> and accepts it …
        assert_eq!(
            strip_unsupported_marks("roses <bookmark mark='f1'/> and", false),
            "roses <bookmark mark='f1'/> and"
        );
        // … but the free Edge endpoint synthesises zero audio for it, so
        // the Edge path strips it too (verified live).
        assert_eq!(
            strip_unsupported_marks("roses <bookmark mark='f1'/> and", true),
            "roses  and"
        );
    }

    #[test]
    pub(crate) fn test_express_as_kept_for_azure_tags_dropped_for_edge() {
        // SpeechMarkdown #[style] sections become mstts:express-as wrappers.
        let styled = "<mstts:express-as style=\"angry\">I am angry!</mstts:express-as>";
        assert_eq!(strip_unsupported_marks(styled, false), styled);
        // Edge zero-audios on express-as; the tags go, the spoken text stays.
        assert_eq!(strip_unsupported_marks(styled, true), "I am angry!");
        // Sibling mstts elements are not touched.
        assert_eq!(
            strip_unsupported_marks("<mstts:backgroundaudio src='x'/>", true),
            "<mstts:backgroundaudio src='x'/>"
        );
    }

    // ===== inject_voice_if_missing =====

    #[test]
    pub(crate) fn test_inject_voice_when_ssml_has_no_voice_tag() {
        let ssml = "<speak><prosody rate='slow'>Hello</prosody></speak>";
        let result = inject_voice_if_missing(ssml, "en-GB-AbbiNeural");
        assert!(
            result.contains("<voice name='en-GB-AbbiNeural'>"),
            "voice tag should be injected"
        );
        assert!(
            result.contains("<prosody rate='slow'>Hello</prosody>"),
            "original content should be preserved"
        );
        assert!(
            result.contains("</voice>"),
            "closing voice tag should be added"
        );
    }

    #[test]
    pub(crate) fn test_inject_voice_when_ssml_already_has_voice_tag() {
        let ssml = "<speak><voice name='en-US-Aria'>Hello</voice></speak>";
        let result = inject_voice_if_missing(ssml, "en-GB-AbbiNeural");
        // Should be unchanged — don't inject a second <voice> tag.
        assert_eq!(result, ssml);
    }

    #[test]
    pub(crate) fn test_inject_voice_with_empty_voice_name_is_noop() {
        let ssml = "<speak>Hello</speak>";
        let result = inject_voice_if_missing(ssml, "");
        assert_eq!(result, ssml);
    }

    #[test]
    pub(crate) fn test_inject_voice_with_speak_attributes() {
        // <speak> with version/xmlns attributes should still get the injection
        // after the closing > of the <speak ...> tag.
        let ssml = "<speak version='1.0' xmlns='http://www.w3.org/2001/10/synthesis'>Hello</speak>";
        let result = inject_voice_if_missing(ssml, "en-US-JennyNeural");
        assert!(result.contains("<voice name='en-US-JennyNeural'>"));
        assert!(result.contains("Hello</voice>"));
        assert!(result.contains("</speak>"));
    }

    // ===== SSML passthrough regression =====
    // These tests guard against the bug where tts_speak_ssml SSML was
    // double-wrapped by build_azure_ssml, causing Azure to speak the tags
    // literally. The fix: when is_ssml=true, SSML is passed through directly
    // (not via build_azure_ssml). We verify the invariant by checking that
    // build_azure_ssml DOES escape tags (proving it must NOT be called for
    // raw SSML), and that inject_voice_if_missing does NOT escape them.

    #[test]
    pub(crate) fn test_build_azure_ssml_escapes_tags() {
        // Regression: build_azure_ssml XML-escapes its input. If raw SSML
        // were passed through this function, <prosody> would become
        // &lt;prosody&gt; and Azure would speak it literally.
        let ssml = build_azure_ssml(
            "<prosody rate='slow'>Hello</prosody>",
            "en-US-Aria",
            1.0,
            1.0,
            1.0,
        );
        assert!(
            ssml.contains("&lt;prosody"),
            "build_azure_ssml must escape tags — this proves it must NOT be used for raw SSML"
        );
    }

    #[test]
    pub(crate) fn test_inject_voice_preserves_ssml_tags() {
        // Regression: inject_voice_if_missing (the is_ssml=true path) must
        // NOT escape SSML tags. Otherwise Azure receives literal text.
        let ssml = "<speak><prosody rate='slow'>Hello</prosody></speak>";
        let result = inject_voice_if_missing(ssml, "en-US-Aria");
        assert!(
            !result.contains("&lt;"),
            "SSML tags must NOT be escaped in the is_ssml path"
        );
        assert!(
            result.contains("<prosody rate='slow'>"),
            "SSML tags must be preserved verbatim"
        );
    }

    #[test]
    pub(crate) fn test_build_google_request_basic() {
        let (body, words) = build_google_request("Hello world", "en-US-Wavenet-D", false, None);
        assert_eq!(body["input"]["text"].as_str().unwrap(), "Hello world");
        assert!(words.is_empty());
    }

    #[test]
    pub(crate) fn test_build_google_request_with_ssml_override() {
        // When tts_speak_ssml passes W3C SSML, the <voice> wrapper is stripped
        // and the inner SSML is sent as Google's ssml input.
        let ssml = "<speak>Hello <phoneme alphabet='ipa' ph='wɜːld'>world</phoneme></speak>";
        let (body, words) =
            build_google_request("Hello world", "en-US-Wavenet-D", false, Some(ssml));
        assert!(body["input"]["ssml"].as_str().unwrap().contains("<phoneme"));
        assert!(body["input"]["ssml"].as_str().unwrap().contains("wɜːld"));
        assert!(
            words.is_empty(),
            "no mark-based words when SSML override is used"
        );
    }

    #[test]
    pub(crate) fn test_build_google_request_with_marks() {
        let (body, words) = build_google_request("Hello world", "en-US-Wavenet-D", true, None);
        let ssml = body["input"]["ssml"].as_str().unwrap();
        assert!(ssml.contains("<mark name=\"0\"/>"));
        assert!(ssml.contains("<mark name=\"1\"/>"));
        assert_eq!(words.len(), 2);
        assert_eq!(words[0], "Hello");
        assert_eq!(words[1], "world");
        assert!(body.get("enableTimePointing").is_some());
    }

    #[test]
    pub(crate) fn test_parse_google_timepoints() {
        let tps = vec![
            serde_json::json!({"markName": "0", "timeSeconds": 0.125}),
            serde_json::json!({"markName": "1", "timeSeconds": 0.450}),
        ];
        let words = vec!["Hello".to_string(), "world".to_string()];
        let boundaries = parse_google_timepoints(&tps, &words);
        assert_eq!(boundaries.len(), 2);
        assert_eq!(boundaries[0].text, "Hello");
        assert_eq!(boundaries[0].offset, 125);
        assert_eq!(boundaries[0].duration, 325);
        assert_eq!(boundaries[1].text, "world");
        assert_eq!(boundaries[1].offset, 450);
    }

    #[test]
    pub(crate) fn test_estimate_word_boundaries() {
        let boundaries = estimate_word_boundaries("Hello world this is a test");
        assert_eq!(boundaries.len(), 6);
        assert_eq!(boundaries[0].text, "Hello");
        assert_eq!(boundaries[0].offset, 0);
        assert!(boundaries[0].duration > 0);
    }

    #[test]
    pub(crate) fn test_normalize_gender() {
        assert_eq!(
            super::super::types::normalize_gender("Female"),
            super::super::types::Gender::Female
        );
        assert_eq!(
            super::super::types::normalize_gender("male"),
            super::super::types::Gender::Male
        );
        assert_eq!(
            super::super::types::normalize_gender(""),
            super::super::types::Gender::Unknown
        );
    }

    #[test]
    pub(crate) fn test_build_config_all_engines() {
        // Polly is intentionally omitted — it requires SigV4 and returns None.
        // See test_polly_unsupported_returns_none.
        let engines = [
            "openai",
            "elevenlabs",
            "azure",
            "google",
            "cartesia",
            "deepgram",
            "playht",
            "fishaudio",
            "hume",
            "mistral",
            "murf",
            "resemble",
            "unrealspeech",
            "upliftai",
            "watson",
            "witai",
            "xai",
            "modelslab",
        ];
        let creds = HashMap::new();
        for id in &engines {
            assert!(
                build_config(id, &creds).is_some(),
                "Failed for engine: {id}"
            );
        }
    }

    #[test]
    pub(crate) fn test_build_config_unknown() {
        let creds = HashMap::new();
        assert!(build_config("nonexistent", &creds).is_none());
    }

    #[test]
    pub(crate) fn test_azure_ssml_escapes_special_chars() {
        let ssml = build_azure_ssml("A & B < C > D", "en-US-AriaNeural", 1.0, 1.0, 1.0);
        assert!(ssml.contains("&amp;"));
        assert!(ssml.contains("&lt;"));
        assert!(ssml.contains("&gt;"));
    }

    #[test]
    pub(crate) fn test_azure_ssml_escapes_voice_name() {
        // a stray apostrophe in the voice name must not break the SSML.
        let ssml = build_azure_ssml("hi", "en-US-Voice'Name", 1.0, 1.0, 1.0);
        assert!(
            ssml.contains("&apos;"),
            "voice apostrophe should be escaped"
        );
        assert!(!ssml.contains("Voice'Name"));
    }

    #[test]
    pub(crate) fn test_speech_markdown_preprocessing() {
        use crate::engine::preprocess_speech_markdown;
        let (result, is_ssml) =
            preprocess_speech_markdown("Hello (world)[emphasis:\"strong\"]", "azure");
        assert!(is_ssml);
        // The patched speechmarkdown-rust (v0.4.10+) adds Azure-required
        // attributes (version, xmlns, xml:lang) to the <speak> tag.
        assert!(
            result.contains("<speak "),
            "expected <speak> with attributes: {result}"
        );
        assert!(
            result.contains("version="),
            "missing version attr: {result}"
        );
        assert!(
            result.contains("www.w3.org/2001/10/synthesis"),
            "missing xmlns attr: {result}"
        );
    }

    #[test]
    pub(crate) fn test_watson_auth_header_format() {
        // IBM Watson Basic auth requires the literal username `apikey`
        // (lowercase, one word) per the IAM authentication spec. Earlier
        // versions of this code used `apiKey` (camelCase) and got HTTP 401
        // from every Watson endpoint. This regression test pins the
        // lowercase form.
        use base64::Engine as _;
        let api_key = "test_key_123";
        let encoded = base64_encode(&format!("apikey:{api_key}"));
        let auth_header = format!("Basic {encoded}");
        assert!(!auth_header.ends_with(':'));
        let decoded = String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(encoded.as_bytes())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(decoded, format!("apikey:{api_key}"));
        // Explicit guard against the camelCase regression.
        assert!(
            !decoded.starts_with("apiKey:"),
            "Watson auth must use lowercase 'apikey:' as the username, got: {decoded}"
        );
    }

    #[test]
    pub(crate) fn test_playht_config_has_user_id_header() {
        // userId belongs in the X-User-ID header, not the JSON body.
        let mut creds = HashMap::new();
        creds.insert("userId".to_string(), "u-123".to_string());
        creds.insert("apiKey".to_string(), "k".to_string());
        let cfg = build_config("playht", &creds).expect("playht config");
        assert_eq!(
            cfg.extra_headers.get("X-User-ID").map(String::as_str),
            Some("u-123")
        );
        // The voice param stays in the body, not the headers.
        assert_eq!(cfg.voice_param, "voice");
    }

    #[test]
    pub(crate) fn test_deepgram_uses_model_param() {
        // Deepgram's /v1/speak takes `model` as the voice parameter.
        let creds = HashMap::new();
        let cfg = build_config("deepgram", &creds).expect("deepgram config");
        assert_eq!(cfg.voice_param, "model");
    }

    #[test]
    pub(crate) fn test_edge_config_is_credential_free_ws() {
        // Edge is free + WS-only: no synth REST URL, no auth header, but it
        // must expose the bing.com voice list and a default voice.
        let cfg = build_config("edge", &HashMap::new()).expect("edge config");
        assert_eq!(cfg.provider_id, "edge");
        assert!(cfg.synth_url.is_empty());
        assert!(cfg.auth_header.is_empty());
        assert!(cfg
            .voices_url
            .as_deref()
            .unwrap()
            .contains("speech.platform.bing.com"));
        assert_eq!(cfg.default_voice.as_deref(), Some("en-US-AriaNeural"));
        // Edge returns MP3 on its free endpoint — the WS loop decodes it.
        assert!(!cfg.response_is_pcm);
    }

    #[test]
    pub(crate) fn test_edge_sec_ms_gec_is_uppercase_hex_sha256() {
        // Sec-MS-GEC is SHA-256 → 32 bytes → 64 uppercase hex chars.
        let token = edge_sec_ms_gec();
        assert_eq!(token.len(), 64, "token must be 64 hex chars: {token}");
        assert!(
            token
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()),
            "token must be uppercase hex: {token}"
        );
    }

    #[test]
    pub(crate) fn test_edge_sec_ms_gec_is_stable_within_five_minutes() {
        // The token is rounded down to a 5-minute window, so two calls within
        // the same window must yield identical tokens (guards the rounding).
        let a = edge_sec_ms_gec();
        let b = edge_sec_ms_gec();
        assert_eq!(a, b, "tokens within the same 5-minute window must match");
    }

    #[test]
    pub(crate) fn test_hume_voice_is_object_in_extra_body() {
        // Hume expects voice as {"voice": {"name": "..."}}.
        let creds = HashMap::new();
        let cfg = build_config("hume", &creds).expect("hume config");
        let voice = cfg
            .extra_body
            .get("voice")
            .expect("voice key should be in extra_body");
        assert!(voice.is_object(), "voice must be an object, got: {voice}");
        assert!(voice.get("name").is_some());
        assert_eq!(
            cfg.extra_body.get("audio_format").and_then(|v| v.as_str()),
            Some("wav")
        );
    }

    #[test]
    pub(crate) fn test_polly_unsupported_returns_none() {
        // AWS Polly needs SigV4. We surface this by returning None
        // (and emitting a warning) rather than constructing a broken config.
        let creds = HashMap::new();
        assert!(build_config("polly", &creds).is_none());
    }

    // ===== Per-engine config matrix =====
    //
    // One test per provider asserting the URL the engine will actually hit,
    // the auth header scheme, the JSON body shape, and (where applicable) the
    // voice-listing URL. Regression coverage for the kind of auth/URL bugs
    // that bit Watson, PlayHT, Deepgram, and Hume in earlier revisions.

    pub(crate) fn engine_creds(id: &str) -> HashMap<String, String> {
        let mut c = HashMap::new();
        c.insert("apiKey".to_string(), "TESTKEY".to_string());
        match id {
            "azure" => {
                c.insert("subscriptionKey".to_string(), "TESTKEY".to_string());
                c.insert("region".to_string(), "eastus".to_string());
            }
            "watson" => {
                c.insert("region".to_string(), "eu-gb".to_string());
                c.insert("instanceId".to_string(), "inst-123".to_string());
            }
            "playht" => {
                c.insert("userId".to_string(), "u-123".to_string());
            }
            "hume" => {
                c.insert("voice".to_string(), "aoife".to_string());
            }
            _ => {}
        }
        c
    }

    #[test]
    pub(crate) fn test_openai_config_matrix() {
        let cfg = build_config("openai", &engine_creds("openai")).expect("openai");
        assert_eq!(cfg.synth_url, "https://api.openai.com/v1/audio/speech");
        assert_eq!(cfg.auth_header, "Authorization");
        assert_eq!(cfg.auth_prefix, "Bearer ");
        assert_eq!(cfg.text_field, "input");
        assert_eq!(cfg.voice_param, "voice");
        assert_eq!(cfg.model_param.as_deref(), Some("model"));
        assert_eq!(cfg.model_default.as_deref(), Some("gpt-4o-mini-tts"));
        assert_eq!(cfg.default_voice.as_deref(), Some("alloy"));
        assert_eq!(cfg.provider_id, "openai");
        assert!(!cfg.body_is_ssml);
    }

    #[test]
    pub(crate) fn test_elevenlabs_config_matrix() {
        let cfg = build_config("elevenlabs", &engine_creds("elevenlabs")).expect("elevenlabs");
        // Default voice_id is baked into the synth URL path.
        assert!(cfg
            .synth_url
            .starts_with("https://api.elevenlabs.io/v1/text-to-speech/"));
        assert_eq!(cfg.auth_header, "xi-api-key");
        assert_eq!(cfg.auth_prefix, "");
        assert_eq!(cfg.text_field, "text");
        assert_eq!(cfg.model_param.as_deref(), Some("model_id"));
        assert_eq!(
            cfg.voices_url.as_deref(),
            Some("https://api.elevenlabs.io/v1/voices")
        );
    }

    #[test]
    pub(crate) fn test_elevenlabs_voice_id_from_creds() {
        let mut c = engine_creds("elevenlabs");
        c.insert("voiceId".to_string(), "v-abc".to_string());
        let cfg = build_config("elevenlabs", &c).expect("elevenlabs");
        assert!(cfg.synth_url.ends_with("/text-to-speech/v-abc"));
    }

    #[test]
    pub(crate) fn test_azure_config_matrix() {
        let cfg = build_config("azure", &engine_creds("azure")).expect("azure");
        assert_eq!(
            cfg.synth_url,
            "https://eastus.tts.speech.microsoft.com/cognitiveservices/v1"
        );
        assert_eq!(cfg.auth_header, "Ocp-Apim-Subscription-Key");
        assert_eq!(cfg.auth_prefix, "");
        assert!(cfg.body_is_ssml);
        assert_eq!(cfg.content_type.as_deref(), Some("application/ssml+xml"));
        assert_eq!(
            cfg.voices_url.as_deref(),
            Some("https://eastus.tts.speech.microsoft.com/cognitiveservices/voices/list")
        );
        assert_eq!(
            cfg.extra_headers
                .get("X-Microsoft-OutputFormat")
                .map(String::as_str),
            // Raw PCM16 24 kHz mono — flows straight to on_audio without an
            // MP3 decode step (uniform PCM contract across all engines).
            Some("raw-24khz-16bit-mono-pcm")
        );
        assert_eq!(cfg.default_voice.as_deref(), Some("en-US-AriaNeural"));
    }

    #[test]
    pub(crate) fn test_azure_region_override() {
        let mut c = engine_creds("azure");
        c.insert("region".to_string(), "uksouth".to_string());
        let cfg = build_config("azure", &c).expect("azure");
        assert!(cfg.synth_url.starts_with("https://uksouth.tts."));
        assert!(cfg
            .voices_url
            .as_deref()
            .unwrap()
            .starts_with("https://uksouth.tts."));
    }

    #[test]
    pub(crate) fn test_google_config_matrix() {
        let cfg = build_config("google", &engine_creds("google")).expect("google");
        // The API key must be embedded as ?key= in both URLs.
        assert!(cfg
            .synth_url
            .starts_with("https://texttospeech.googleapis.com/v1/text:synthesize?key="));
        assert!(cfg.synth_url.ends_with("TESTKEY"));
        assert!(cfg
            .voices_url
            .as_deref()
            .unwrap()
            .starts_with("https://texttospeech.googleapis.com/v1/voices?key="));
    }

    #[test]
    pub(crate) fn test_cartesia_config_matrix() {
        let cfg = build_config("cartesia", &engine_creds("cartesia")).expect("cartesia");
        assert_eq!(cfg.synth_url, "https://api.cartesia.ai/tts/bytes");
        assert_eq!(cfg.auth_header, "X-API-Key");
        assert_eq!(cfg.voice_param, "voice_id");
        assert_eq!(cfg.model_default.as_deref(), Some("sonic-2"));
        assert_eq!(
            cfg.voices_url.as_deref(),
            Some("https://api.cartesia.ai/voices")
        );
    }

    #[test]
    pub(crate) fn test_deepgram_config_matrix() {
        let cfg = build_config("deepgram", &engine_creds("deepgram")).expect("deepgram");
        assert_eq!(cfg.synth_url, "https://api.deepgram.com/v1/speak");
        assert_eq!(cfg.auth_header, "Authorization");
        assert_eq!(cfg.auth_prefix, "Token ");
        assert_eq!(cfg.voice_param, "model");
        assert_eq!(cfg.default_voice.as_deref(), Some("aura-asteria-en"));
        assert_eq!(
            cfg.voices_url.as_deref(),
            Some("https://api.deepgram.com/v1/voices")
        );
    }

    #[test]
    pub(crate) fn test_playht_config_matrix() {
        let cfg = build_config("playht", &engine_creds("playht")).expect("playht");
        assert_eq!(cfg.synth_url, "https://api.play.ht/api/v2/tts");
        assert_eq!(cfg.auth_header, "Authorization");
        assert_eq!(cfg.auth_prefix, "Bearer ");
        assert_eq!(
            cfg.extra_headers.get("X-User-ID").map(String::as_str),
            Some("u-123")
        );
        assert_eq!(
            cfg.voices_url.as_deref(),
            Some("https://api.play.ht/api/v2/voices")
        );
    }

    #[test]
    pub(crate) fn test_fishaudio_config_matrix() {
        let cfg = build_config("fishaudio", &engine_creds("fishaudio")).expect("fishaudio");
        assert_eq!(cfg.synth_url, "https://api.fish.audio/v1/tts");
        assert_eq!(cfg.auth_header, "Authorization");
        assert_eq!(cfg.auth_prefix, "Bearer ");
        assert_eq!(cfg.voice_param, "reference_id");
    }

    #[test]
    pub(crate) fn test_hume_config_matrix() {
        let cfg = build_config("hume", &engine_creds("hume")).expect("hume");
        assert_eq!(cfg.synth_url, "https://api.hume.ai/v0/tts");
        assert_eq!(cfg.auth_header, "Authorization");
        assert_eq!(cfg.auth_prefix, "Bearer ");
        // voice is in extra_body, not voice_param.
        assert_eq!(cfg.voice_param, "");
        // The supplied voice name lands in the nested object.
        assert_eq!(
            cfg.extra_body
                .get("voice")
                .and_then(|v| v.get("name"))
                .and_then(|v| v.as_str()),
            Some("aoife")
        );
    }

    #[test]
    pub(crate) fn test_mistral_config_matrix() {
        let cfg = build_config("mistral", &engine_creds("mistral")).expect("mistral");
        assert_eq!(cfg.synth_url, "https://api.mistral.ai/v1/tts");
        assert_eq!(cfg.text_field, "text");
        assert_eq!(cfg.voice_param, "voice");
    }

    #[test]
    pub(crate) fn test_murf_config_matrix() {
        let cfg = build_config("murf", &engine_creds("murf")).expect("murf");
        assert_eq!(cfg.synth_url, "https://api.murf.ai/v1/speech/generate");
        assert_eq!(cfg.auth_header, "api-key");
        assert_eq!(cfg.auth_prefix, "");
        assert_eq!(cfg.voice_param, "voice_id");
    }

    #[test]
    pub(crate) fn test_resemble_config_matrix() {
        let cfg = build_config("resemble", &engine_creds("resemble")).expect("resemble");
        assert_eq!(cfg.synth_url, "https://app.resemble.ai/api/v2/synthesize");
        assert_eq!(cfg.auth_header, "Authorization");
        assert_eq!(cfg.auth_prefix, "Token ");
        assert_eq!(cfg.voice_param, "voice_uuid");
        assert_eq!(
            cfg.voices_url.as_deref(),
            Some("https://app.resemble.ai/api/v2/voices")
        );
    }

    #[test]
    pub(crate) fn test_unrealspeech_config_matrix() {
        let cfg =
            build_config("unrealspeech", &engine_creds("unrealspeech")).expect("unrealspeech");
        assert_eq!(cfg.synth_url, "https://api.v7.unrealspeech.com/speech");
        assert_eq!(cfg.default_voice.as_deref(), Some("Scarlett"));
        assert_eq!(cfg.voice_param, "voice_id");
    }

    #[test]
    pub(crate) fn test_upliftai_config_matrix() {
        let cfg = build_config("upliftai", &engine_creds("upliftai")).expect("upliftai");
        assert_eq!(cfg.synth_url, "https://api.upliftai.org/v1/tts");
    }

    #[test]
    pub(crate) fn test_watson_config_matrix() {
        let cfg = build_config("watson", &engine_creds("watson")).expect("watson");
        assert!(cfg
            .synth_url
            .starts_with("https://eu-gb.text-to-speech.watson.cloud.ibm.com/instances/inst-123/"));
        assert!(cfg.synth_url.ends_with("/v1/synthesize"));
        assert_eq!(cfg.auth_header, "Authorization");
        // Basic auth — base64("apiKey:TESTKEY"), prefix "Basic ".
        assert!(cfg.auth_prefix.starts_with("Basic "));
        // Round-trip the base64 to confirm the credentials are encoded in
        // the documented `apiKey:<key>` shape (not `<key>:` as it once was).
        let encoded = cfg.auth_prefix.strip_prefix("Basic ").unwrap();
        let decoded = {
            use base64::Engine;
            String::from_utf8(
                base64::engine::general_purpose::STANDARD
                    .decode(encoded.as_bytes())
                    .unwrap(),
            )
            .unwrap()
        };
        assert_eq!(decoded, "apikey:TESTKEY");
        assert!(cfg.voices_url.as_deref().unwrap().ends_with("/v1/voices"));
    }

    #[test]
    pub(crate) fn test_witai_config_matrix() {
        let cfg = build_config("witai", &engine_creds("witai")).expect("witai");
        assert_eq!(cfg.synth_url, "https://api.wit.ai/synthesize?v=20240304");
        assert_eq!(cfg.auth_header, "Authorization");
        assert_eq!(cfg.auth_prefix, "Bearer ");
        assert_eq!(
            cfg.voices_url.as_deref(),
            Some("https://api.wit.ai/voices?v=20240304")
        );
    }

    #[test]
    pub(crate) fn test_xai_config_matrix() {
        let cfg = build_config("xai", &engine_creds("xai")).expect("xai");
        assert_eq!(cfg.synth_url, "https://api.x.ai/v1/audio/speech");
        // xAI uses `input` like OpenAI, not `text`.
        assert_eq!(cfg.text_field, "input");
        assert_eq!(cfg.voice_param, "voice");
    }

    #[test]
    pub(crate) fn test_modelslab_config_matrix() {
        let cfg = build_config("modelslab", &engine_creds("modelslab")).expect("modelslab");
        assert_eq!(cfg.synth_url, "https://modelslab.com/api/v1/text_to_speech");
        // ModelsLab has no auth header — key goes in the body by convention.
        assert_eq!(cfg.auth_header, "");
    }

    // ===== Static voice lists =====

    #[test]
    pub(crate) fn test_static_voice_counts() {
        assert_eq!(static_voices("openai").unwrap().len(), 11);
        assert_eq!(static_voices("hume").unwrap().len(), 16);
        assert_eq!(static_voices("mistral").unwrap().len(), 27);
        assert_eq!(static_voices("murf").unwrap().len(), 15);
        assert_eq!(static_voices("unrealspeech").unwrap().len(), 8);
        assert_eq!(static_voices("xai").unwrap().len(), 6);
        assert_eq!(static_voices("upliftai").unwrap().len(), 4);
        assert_eq!(static_voices("modelslab").unwrap().len(), 9);
    }

    #[test]
    pub(crate) fn test_static_voices_returns_none_for_api_engines() {
        // These engines fetch from an API, not a static list.
        assert!(static_voices("azure").is_none());
        assert!(static_voices("google").is_none());
        assert!(static_voices("elevenlabs").is_none());
        assert!(static_voices("deepgram").is_none());
        assert!(static_voices("playht").is_none());
        assert!(static_voices("fishaudio").is_none());
        assert!(static_voices("watson").is_none());
        assert!(static_voices("witai").is_none());
        assert!(static_voices("resemble").is_none());
    }

    // ===== Voice-list response parsers =====

    #[test]
    pub(crate) fn test_map_azure_voices_basic() {
        let json = serde_json::json!([{
            "ShortName": "en-US-AriaNeural",
            "DisplayName": "Aria",
            "Gender": "Female",
            "Locale": "en-US",
            "LocaleName": "English (United States)"
        }]);
        let voices = map_azure_voices(json.as_array().unwrap());
        assert_eq!(voices.len(), 1);
        assert_eq!(voices[0].id, "en-US-AriaNeural");
        assert_eq!(voices[0].name, "Aria");
        assert_eq!(voices[0].gender, crate::types::Gender::Female);
        assert_eq!(voices[0].provider, "azure");
        assert_eq!(voices[0].language_codes[0].bcp47, "en-US");
        assert_eq!(voices[0].language_codes[0].iso639_3, "en");
    }

    #[test]
    pub(crate) fn test_map_azure_voices_skips_missing_short_name() {
        // Voices without ShortName shouldn't parse — defensive against
        // Azure adding new object shapes.
        let json = serde_json::json!([
            {"DisplayName": "NoShortName"},
            {"ShortName": "en-US-GuyNeural", "Gender": "Male", "Locale": "en-US"}
        ]);
        let voices = map_azure_voices(json.as_array().unwrap());
        assert_eq!(voices.len(), 1);
        assert_eq!(voices[0].id, "en-US-GuyNeural");
    }

    #[test]
    pub(crate) fn test_map_google_voices_basic() {
        let json = serde_json::json!([{
            "name": "en-US-Wavenet-D",
            "ssmlGender": "MALE",
            "languageCodes": ["en-US", "en-GB"]
        }]);
        let voices = map_google_voices(json.as_array().unwrap());
        assert_eq!(voices.len(), 1);
        assert_eq!(voices[0].id, "en-US-Wavenet-D");
        assert_eq!(voices[0].gender, crate::types::Gender::Male);
        assert_eq!(voices[0].language_codes.len(), 2);
    }

    #[test]
    pub(crate) fn test_map_google_voices_missing_name_skipped() {
        let json = serde_json::json!([{"ssmlGender": "FEMALE"}]);
        assert!(map_google_voices(json.as_array().unwrap()).is_empty());
    }

    #[test]
    pub(crate) fn test_map_generic_voices_elevenlabs_labels_object() {
        // ElevenLabs stores gender/language inside a nested `labels` object.
        let json = serde_json::json!([{
            "voice_id": "21m00Tcm4TlvDq8ikWAM",
            "name": "Rachel",
            "labels": {"gender": "female", "language": "en"}
        }]);
        let voices = map_generic_voices("elevenlabs", json.as_array().unwrap());
        assert_eq!(voices.len(), 1);
        assert_eq!(voices[0].id, "21m00Tcm4TlvDq8ikWAM");
        assert_eq!(voices[0].name, "Rachel");
        assert_eq!(voices[0].provider, "elevenlabs");
        assert_eq!(voices[0].gender, crate::types::Gender::Female);
        assert_eq!(voices[0].language_codes[0].bcp47, "en");
    }

    #[test]
    pub(crate) fn test_map_generic_voices_polly_pascal_case() {
        // Polly DescribeVoices returns VoiceId / Gender / LanguageCode.
        let json = serde_json::json!([{
            "VoiceId": "Joanna",
            "Gender": "Female",
            "LanguageCode": "en-US"
        }]);
        let voices = map_generic_voices("polly", json.as_array().unwrap());
        assert_eq!(voices.len(), 1);
        assert_eq!(voices[0].id, "Joanna");
        assert_eq!(voices[0].gender, crate::types::Gender::Female);
        assert_eq!(voices[0].language_codes[0].bcp47, "en-US");
    }

    #[test]
    pub(crate) fn test_map_generic_voices_cartesia_simple() {
        let json = serde_json::json!([{
            "id": "692f0249-6e6b-4a48-8b07-0f8f8a3f3a15",
            "name": "Octopus"
        }]);
        let voices = map_generic_voices("cartesia", json.as_array().unwrap());
        assert_eq!(voices.len(), 1);
        assert_eq!(voices[0].id, "692f0249-6e6b-4a48-8b07-0f8f8a3f3a15");
        assert_eq!(voices[0].name, "Octopus");
        assert!(voices[0].language_codes.is_empty());
    }

    #[test]
    pub(crate) fn test_map_generic_voices_skips_no_id() {
        // Voice without any id-like field is skipped; `name` alone is enough
        // to use as the id fallback (see id-resolution order).
        let json = serde_json::json!([
            {"category": "voiceless"}, // no id/voice_id/VoiceId/name/Name
            {"id": "ok", "name": "OK"}
        ]);
        let voices = map_generic_voices("test", json.as_array().unwrap());
        assert_eq!(voices.len(), 1);
        assert_eq!(voices[0].id, "ok");
    }

    #[test]
    pub(crate) fn test_map_generic_voices_provider_tagged() {
        // Each call must tag the voice with the provider string passed in.
        let json = serde_json::json!([{"id": "x", "name": "X"}]);
        for provider in ["openai", "murf", "resemble", "witai"] {
            let voices = map_generic_voices(provider, json.as_array().unwrap());
            assert_eq!(voices[0].provider, provider);
        }
    }

    // ===== compute_durations =====

    #[test]
    pub(crate) fn test_compute_durations_empty_no_panic() {
        let mut v: Vec<WordBoundary> = vec![];
        compute_durations(&mut v);
    }

    #[test]
    pub(crate) fn test_compute_durations_single_entry_floor_500ms() {
        let mut v = vec![WordBoundary {
            text: "Hi".into(),
            offset: 0,
            duration: 0,
            estimated: false,
        }];
        compute_durations(&mut v);
        assert_eq!(v[0].duration, 500);
    }

    #[test]
    pub(crate) fn test_compute_distributions_fills_zero_from_next_offset() {
        let mut v = vec![
            WordBoundary {
                text: "a".into(),
                offset: 0,
                duration: 0,
                estimated: false,
            },
            WordBoundary {
                text: "b".into(),
                offset: 300,
                duration: 0,
                estimated: false,
            },
            WordBoundary {
                text: "c".into(),
                offset: 700,
                duration: 0,
                estimated: false,
            },
        ];
        compute_durations(&mut v);
        assert_eq!(v[0].duration, 300); // next.offset - this.offset
        assert_eq!(v[1].duration, 400);
        assert_eq!(v[2].duration, 500); // last entry floor
    }

    #[test]
    pub(crate) fn test_compute_durations_preserves_nonzero() {
        let mut v = vec![
            WordBoundary {
                text: "a".into(),
                offset: 0,
                duration: 250,
                estimated: false,
            },
            WordBoundary {
                text: "b".into(),
                offset: 250,
                duration: 0,
                estimated: false,
            },
        ];
        compute_durations(&mut v);
        assert_eq!(v[0].duration, 250); // untouched
    }

    // ===== ElevenLabs alignment parser =====

    #[test]
    pub(crate) fn test_parse_elevenlabs_alignment_basic_words() {
        // "Hello world" — characters with a space separator in the middle.
        let mut alignment = serde_json::Map::new();
        alignment.insert(
            "characters".into(),
            serde_json::json!(["H", "e", "l", "l", "o", " ", "w", "o", "r", "l", "d"]),
        );
        alignment.insert(
            "character_start_times_seconds".into(),
            serde_json::json!([0.0, 0.05, 0.10, 0.15, 0.20, 0.25, 0.30, 0.35, 0.40, 0.45, 0.50]),
        );
        alignment.insert(
            "character_end_times_seconds".into(),
            serde_json::json!([0.05, 0.10, 0.15, 0.20, 0.25, 0.30, 0.35, 0.40, 0.45, 0.50, 0.55]),
        );

        let words = parse_elevenlabs_alignment(&alignment);
        assert_eq!(words.len(), 2);
        assert_eq!(words[0].0, "Hello");
        assert!((words[0].1 - 0.0).abs() < f32::EPSILON);
        assert!((words[0].2 - 0.30).abs() < f32::EPSILON); // ends at space's end_time
        assert_eq!(words[1].0, "world");
        assert!((words[1].1 - 0.30).abs() < f32::EPSILON);
        assert!((words[1].2 - 0.55).abs() < f32::EPSILON); // last char's end
    }

    #[test]
    pub(crate) fn test_parse_elevenlabs_alignment_handles_mismatched_arrays() {
        // characters has more entries than the time arrays — must not panic.
        let mut alignment = serde_json::Map::new();
        alignment.insert(
            "characters".into(),
            serde_json::json!(["H", "e", "l", "l", "o"]),
        );
        alignment.insert(
            "character_start_times_seconds".into(),
            serde_json::json!([0.0, 0.05]), // short
        );
        alignment.insert(
            "character_end_times_seconds".into(),
            serde_json::json!([0.05, 0.10]), // short
        );

        let words = parse_elevenlabs_alignment(&alignment);
        // The trailing characters with no time data get folded into one word
        // whose end is `ends.last()` — defensive but consistent.
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].0, "Hello");
    }

    #[test]
    pub(crate) fn test_parse_elevenlabs_alignment_missing_arrays_returns_empty() {
        let alignment = serde_json::Map::new(); // no keys
        assert!(parse_elevenlabs_alignment(&alignment).is_empty());
    }

    // ===== Azure WS message parser =====

    #[test]
    pub(crate) fn test_looks_like_mp3_id3_tag() {
        assert!(looks_like_mp3(b"ID3\x03\x00\x00\x00\x00"));
    }

    #[test]
    pub(crate) fn test_looks_like_mp3_frame_sync() {
        // First byte 0xFF, second with top 3 bits set (MPEG sync).
        assert!(looks_like_mp3(&[0xFF, 0xE3, 0x10, 0x00]));
        assert!(looks_like_mp3(&[0x00, 0x00, 0xFF, 0xFB, 0x90])); // sync mid-stream
    }

    #[test]
    pub(crate) fn test_looks_like_mp3_raw_pcm_is_false() {
        // Raw PCM16 has no sync word / ID3 — must not be mistaken for MP3.
        assert!(!looks_like_mp3(&[0x00, 0x01, 0x02, 0x03, 0x04, 0x05]));
        assert!(!looks_like_mp3(&[]));
    }

    #[test]
    pub(crate) fn test_decode_mp3_garbage_returns_empty_without_panicking() {
        // Empty / non-MP3 input must not panic and must yield no PCM.
        assert!(decode_mp3_to_pcm16_mono(&[]).is_empty());
        assert!(decode_mp3_to_pcm16_mono(b"definitely not mp3").is_empty());
    }

    #[test]
    pub(crate) fn test_azure_ws_extract_path_basic() {
        let frame = "X-RequestId:abc\r\nPath:turn.end\r\nContent-Type:application/json\r\n\r\n{}";
        assert_eq!(azure_ws_extract_path(frame), "turn.end");
    }

    #[test]
    pub(crate) fn test_azure_ws_extract_path_missing_returns_empty() {
        let frame = "X-RequestId:abc\r\nContent-Type:application/json\r\n\r\n{}";
        assert_eq!(azure_ws_extract_path(frame), "");
    }

    #[test]
    pub(crate) fn test_azure_ws_extract_path_trims_whitespace() {
        let frame = "Path:   audio.metadata   \r\n\r\n{}";
        assert_eq!(azure_ws_extract_path(frame), "audio.metadata");
    }

    #[test]
    pub(crate) fn test_azure_ws_extract_path_unicode_does_not_panic() {
        // The original byte-slicing version panicked on non-ASCII. Verify the
        // lines()-based version handles UTF-8 cleanly.
        let frame = "Path:tëst\r\n\r\n{}";
        assert_eq!(azure_ws_extract_path(frame), "tëst");
    }

    #[test]
    pub(crate) fn test_azure_ws_extract_body_crlf_separator() {
        let frame = "X-RequestId:abc\r\nPath:response\r\n\r\n{\"Error\":{\"Message\":\"nope\"}}";
        assert_eq!(
            azure_ws_extract_body(frame),
            "{\"Error\":{\"Message\":\"nope\"}}"
        );
    }

    #[test]
    pub(crate) fn test_azure_ws_extract_body_lf_separator() {
        // Some intermediaries collapse \r\n\r\n to \n\n. Accept it.
        let frame = "X-RequestId:abc\nPath:response\n\n{}";
        assert_eq!(azure_ws_extract_body(frame), "{}");
    }

    #[test]
    pub(crate) fn test_azure_ws_extract_body_missing_returns_empty() {
        assert_eq!(azure_ws_extract_body("just headers no body"), "");
    }

    #[test]
    pub(crate) fn test_azure_ws_extract_error_message_field() {
        let body = r#"{"Error":{"Message":"Authentication failed"}}"#;
        assert_eq!(
            azure_ws_extract_error(body).as_deref(),
            Some("Authentication failed")
        );
    }

    #[test]
    pub(crate) fn test_azure_ws_extract_error_reason_fallback() {
        let body = r#"{"Error":{}, "reason":"queued full"}"#;
        assert_eq!(azure_ws_extract_error(body).as_deref(), Some("queued full"));
    }

    #[test]
    pub(crate) fn test_azure_ws_extract_error_default_when_no_message() {
        let body = r#"{"Error":{"Code":"SynthesisFailed"}}"#;
        assert_eq!(
            azure_ws_extract_error(body).as_deref(),
            Some("Azure synthesis failed")
        );
    }

    #[test]
    pub(crate) fn test_azure_ws_extract_error_none_on_success_response() {
        // turn.end / successful response bodies don't carry `Error`.
        assert!(azure_ws_extract_error("{}").is_none());
        assert!(azure_ws_extract_error(r#"{"foo":"bar"}"#).is_none());
    }

    #[test]
    pub(crate) fn test_azure_ws_extract_error_invalid_json_returns_none() {
        assert!(azure_ws_extract_error("not json").is_none());
    }

    #[test]
    pub(crate) fn test_azure_ws_parse_word_boundary_current_shape() {
        // Current Azure shape: text is a nested object {"text": {"Text": "Hello"}}.
        let item = serde_json::json!({
            "Type": "WordBoundary",
            "Data": {
                "Offset": 500_000,     // 50 ms in ticks
                "Duration": 2_500_000,  // 250 ms in ticks
                "text": {"Text": "Hello"}
            }
        });
        let (word, offset_ms, duration_ms, char_offset, char_len) =
            azure_ws_parse_word_boundary(&item).expect("parsed");
        assert_eq!(word, "Hello");
        assert_eq!(offset_ms, 50);
        assert_eq!(duration_ms, 250);
        // No Offset/Length in the text object → -1.
        assert_eq!(char_offset, -1);
        assert_eq!(char_len, -1);
    }

    #[test]
    pub(crate) fn test_azure_ws_parse_word_boundary_with_text_offset() {
        // Azure WS sends character offset and length in the nested text object.
        let item = serde_json::json!({
            "Type": "WordBoundary",
            "Data": {
                "Offset": 2_800_000,
                "Duration": 1_500_000,
                "text": {"Text": "quick", "Offset": 4, "Length": 5}
            }
        });
        let (word, _, _, char_offset, char_len) =
            azure_ws_parse_word_boundary(&item).expect("parsed");
        assert_eq!(word, "quick");
        assert_eq!(char_offset, 4);
        assert_eq!(char_len, 5);
    }

    #[test]
    pub(crate) fn test_azure_ws_parse_word_boundary_legacy_capital_t() {
        let item = serde_json::json!({
            "Type": "WordBoundary",
            "Data": {
                "Offset": 0,
                "Duration": 100_000,
                "Text": {"Text": "Hi"}
            }
        });
        let (word, _, _, _, _) = azure_ws_parse_word_boundary(&item).expect("parsed");
        assert_eq!(word, "Hi");
    }

    #[test]
    pub(crate) fn test_azure_ws_parse_word_boundary_flat_string() {
        let item = serde_json::json!({
            "Type": "WordBoundary",
            "Data": {"Offset": 0, "Duration": 0, "text": "Yo"}
        });
        let (word, _, _, _, _) = azure_ws_parse_word_boundary(&item).expect("parsed");
        assert_eq!(word, "Yo");
    }

    #[test]
    pub(crate) fn test_azure_ws_parse_word_boundary_filters_empty_text() {
        let item = serde_json::json!({
            "Type": "WordBoundary",
            "Data": {"Offset": 0, "Duration": 0, "text": ""}
        });
        assert!(azure_ws_parse_word_boundary(&item).is_none());
    }

    #[test]
    pub(crate) fn test_azure_ws_parse_word_boundary_missing_data() {
        assert!(
            azure_ws_parse_word_boundary(&serde_json::json!({"Type": "WordBoundary"})).is_none()
        );
    }

    #[test]
    pub(crate) fn test_azure_ws_parse_viseme_basic() {
        let item = serde_json::json!({
            "Type": "Viseme",
            "Data": {"VisemeId": 7, "Offset": 25_000_000}  // 2.5s
        });
        let (id, offset_sec) = azure_ws_parse_viseme(&item).expect("parsed");
        assert_eq!(id, 7);
        assert!((offset_sec - 2.5).abs() < 0.01);
    }

    #[test]
    pub(crate) fn test_azure_ws_parse_viseme_missing_data() {
        assert!(azure_ws_parse_viseme(&serde_json::json!({"Type": "Viseme"})).is_none());
    }

    // ===== Streaming chunking & speechmarkdown per-platform =====
    //
    // The speak() loop delivers audio in 8 KB chunks for the JSON-body
    // engines (ElevenLabs `audio_base64`, Google `audioContent`) and via
    // streaming `Read` for everything else. These tests verify the chunk
    // sizing and the SpeechMarkdown → SSML routing that decides which SSML
    // flavour each platform receives.

    // ===== Streaming chunk size regression =====
    //
    // The speak() loop delivers audio via `on_audio` in chunks of
    // STREAMING_CHUNK_SIZE bytes (used by both the base64-decoded JSON
    // payloads and the HTTP streaming-Read path). Pinning the constant
    // catches a future tweak that accidentally switches to e.g. 1024 and
    // creates millions of callback round-trips per request. Earlier
    // versions of this test asserted on `vec.chunks(8192).count()` against
    // a buffer the test itself built — that was tautological (it tested
    // the stdlib, not production code); referencing the constant makes the
    // test catch the actual regression.

    #[test]
    pub(crate) fn test_streaming_chunk_size_constant_value() {
        // Must be a power of two in the KiB range — anything smaller would
        // explode callback count; anything larger would inflate memory.
        assert_eq!(STREAMING_CHUNK_SIZE, 8 * 1024);
        assert!(STREAMING_CHUNK_SIZE.is_power_of_two());
    }

    #[test]
    pub(crate) fn test_streaming_chunk_size_used_in_speak_path() {
        // Defensive: grep-verify the production code references the
        // constant rather than re-introducing a magic 8192 literal. We
        // count uses of `.chunks(STREAMING_CHUNK_SIZE)` in lines that
        // aren't part of this test (the test itself mentions the magic
        // literal in its assertion message, which would false-positive
        // a naive grep).
        let source = include_str!("cloud_engine.rs");
        let production_uses = source
            .lines()
            // Skip every line inside this test's body, which legitimately
            // mentions both .chunks(STREAMING_CHUNK_SIZE) and the magic
            // literal 8192 in its assertion message.
            .filter(|l| !l.contains("test_streaming_chunk_size_used_in_speak_path"))
            .filter(|l| !l.contains("magic 8192"))
            .filter(|l| l.contains(".chunks(STREAMING_CHUNK_SIZE)"))
            .count();
        assert!(
            production_uses >= 2,
            "expected at least 2 production uses of STREAMING_CHUNK_SIZE, found {production_uses}"
        );
    }

    // ===== SpeechMarkdown routing per platform =====
    //
    // speak() calls preprocess_speech_markdown(text, &self.config.provider_id).
    // The cloud engines we route through that helper:
    //   azure   → MicrosoftAzure SSML
    //   google  → GoogleAssistant SSML
    //   *       → AmazonAlexa SSML
    //
    // Verifying the routing requires asserting that the platforms actually
    // produce DIFFERENT output. Every SSML flavour starts with `<speak>`,
    // so an `assert!(ssml.contains("<speak"))` test would still pass if we
    // accidentally routed Azure to the Alexa flavour. Instead we feed the
    // same input through all three platforms and require that at least one
    // differs — that catches a routing collapse in either direction.

    #[test]
    pub(crate) fn test_speechmarkdown_routing_is_per_platform() {
        // Verify the preprocess_speech_markdown routing match actually
        // dispatches to different Platform variants. Some SpeechMarkdown
        // constructs produce identical SSML across all platforms (the
        // library normalises a common subset), so a single-input test
        // can't distinguish "routing works" from "routing collapsed but
        // the library happens to emit the same bytes". We try several
        // constructs that have historically differed between Microsoft /
        // Google / Alexa flavours and require at least one to produce
        // distinct output across azure/google/other.
        use crate::engine::preprocess_speech_markdown;
        let probe_inputs = [
            "(world)[emphasis:\"strong\"]",
            "+important+",
            "(world)[rate:\"fast\"]",
            "(world)[pitch:\"high\"]",
            "(world)[volume:\"loud\"]",
            "[rate:\"fast\"]hello[/rate]",
            "This is ^italic^ text",
        ];

        let mut found_distinct = false;
        for input in &probe_inputs {
            let (azure_ssml, azure_ok) = preprocess_speech_markdown(input, "azure");
            let (google_ssml, google_ok) = preprocess_speech_markdown(input, "google");
            let (alexa_ssml, alexa_ok) = preprocess_speech_markdown(input, "openai");

            assert!(azure_ok, "azure failed to parse: {input:?}");
            assert!(google_ok, "google failed to parse: {input:?}");
            assert!(alexa_ok, "alexa failed to parse: {input:?}");

            if azure_ssml != google_ssml || google_ssml != alexa_ssml {
                found_distinct = true;
                break;
            }
        }

        assert!(
            found_distinct,
            "None of {} probe inputs produced distinct SSML across azure/google/alexa. \
             This either means the speechmarkdown-rust library has collapsed its \
             Platform variants to identical output (check the dependency version) or \
             the routing match arm in preprocess_speech_markdown is broken.",
            probe_inputs.len()
        );
    }

    #[test]
    pub(crate) fn test_speechmarkdown_elevenlabs_dialects() {
        use crate::engine::preprocess_speech_markdown;
        // Pre-v3 dialect: <break> prompt markup, not SSML (no <speak>
        // wrapper, is_ssml false so speak() sends it verbatim).
        let (out, is_ssml) = preprocess_speech_markdown("Hello [2s] world", "elevenlabs");
        assert!(!is_ssml, "elevenlabs dialect must not be flagged as SSML");
        assert_eq!(out, "Hello <break time=\"2s\"/> world");

        // v3 dialect: audio tags; no XML the model would read aloud.
        let (out, is_ssml) = preprocess_speech_markdown("Hello [2s] world", "elevenlabs-v3");
        assert!(!is_ssml);
        assert_eq!(out, "Hello [long pause] world");

        let (out, _) = preprocess_speech_markdown("(secret)[whisper]", "elevenlabs-v3");
        assert_eq!(out, "[whispers] secret");

        let (out, _) = preprocess_speech_markdown("(speech)/spitʃ/", "elevenlabs-v3");
        assert_eq!(out, "\"/spitʃ/\"");
    }

    #[test]
    pub(crate) fn test_speechmarkdown_other_providers_detect_input() {
        use crate::engine::preprocess_speech_markdown;
        // OpenAI, Cartesia, Murf, etc. all go through the Alexa fallback.
        // They don't actually consume SSML — the result is discarded by the
        // JSON-body branch in speak() — but detection must still flag the
        // input as SpeechMarkdown so callers querying `is_ssml` get a
        // truthful answer. ElevenLabs is NOT in this list: it gets its own
        // dialects, which are prompt markup, not SSML (see
        // test_speechmarkdown_elevenlabs_dialects).
        for provider in ["openai", "cartesia", "murf", "deepgram", "witai", "xai"] {
            let (_ssml, is_ssml) =
                preprocess_speech_markdown("Hello (world)[emphasis:\"strong\"]", provider);
            assert!(
                is_ssml,
                "provider '{provider}' should detect SpeechMarkdown"
            );
        }
    }

    #[test]
    pub(crate) fn test_speechmarkdown_plain_text_passes_through_unprocessed() {
        use crate::engine::preprocess_speech_markdown;
        for provider in ["azure", "google", "openai", "elevenlabs"] {
            let (out, is_ssml) = preprocess_speech_markdown("Just a plain sentence.", provider);
            assert!(!is_ssml, "provider '{provider}' flagged plain text as SSML");
            assert_eq!(out, "Just a plain sentence.");
        }
    }

    #[test]
    pub(crate) fn test_elevenlabs_synth_url_gains_with_timestamps_when_boundary_requested() {
        // speak() appends `/with-timestamps` to the ElevenLabs synth URL
        // when on_boundary is supplied. We can't drive that branch without
        // a network, but we pin the URL-construction logic so a refactor
        // can't silently drop the suffix.
        let cfg = build_config("elevenlabs", &engine_creds("elevenlabs")).unwrap();
        let mut url = cfg.synth_url.clone();
        url.push_str("/with-timestamps");
        assert!(url.ends_with("/text-to-speech/21m00Tcm4TlvDq8ikWAM/with-timestamps"));
    }

    #[test]
    pub(crate) fn test_elevenlabs_model_id_from_creds() {
        // Default model is eleven_v3, and that default must select the
        // v3 audio-tag dialect — the invariant that makes SpeechMarkdown
        // correct out of the box.
        let cfg = build_config("elevenlabs", &engine_creds("elevenlabs")).unwrap();
        assert_eq!(cfg.model_default.as_deref(), Some("eleven_v3"));
        assert_eq!(
            elevenlabs_smd_platform("elevenlabs", cfg.model_default.as_deref()),
            "elevenlabs-v3"
        );

        // modelId credential overrides it (e.g. a pre-v3 model when
        // <break> markup or long-form character limits are wanted).
        let mut c = engine_creds("elevenlabs");
        c.insert("modelId".into(), "eleven_multilingual_v2".into());
        let cfg = build_config("elevenlabs", &c).unwrap();
        assert_eq!(cfg.model_default.as_deref(), Some("eleven_multilingual_v2"));
        assert_eq!(
            elevenlabs_smd_platform("elevenlabs", cfg.model_default.as_deref()),
            "elevenlabs"
        );

        let mut c = engine_creds("elevenlabs");
        c.insert("modelId".into(), "eleven_flash_v2_5".into());
        let cfg = build_config("elevenlabs", &c).unwrap();
        assert_eq!(cfg.model_default.as_deref(), Some("eleven_flash_v2_5"));

        // Empty modelId falls back to the default.
        let mut c = engine_creds("elevenlabs");
        c.insert("modelId".into(), String::new());
        let cfg = build_config("elevenlabs", &c).unwrap();
        assert_eq!(cfg.model_default.as_deref(), Some("eleven_v3"));
    }

    #[test]
    pub(crate) fn test_elevenlabs_ssml_translates_to_dialects() {
        // W3C/Alexa/Azure-flavoured SSML → SpeechMarkdown → the
        // model-matched ElevenLabs dialect (used by the tts_speak_ssml
        // path instead of stripping).
        let alexa = r#"<speak>Hello <break time="2s"/> world</speak>"#;
        assert_eq!(
            ssml_to_dialect(alexa, "elevenlabs").unwrap(),
            "Hello <break time=\"2s\"/> world"
        );
        let v3 = r#"<speak><amazon:effect name="whispered">secret</amazon:effect></speak>"#;
        assert_eq!(
            ssml_to_dialect(v3, "elevenlabs-v3").unwrap(),
            "[whispers] secret"
        );
        let azure = r#"<speak><mstts:express-as style="cheerful">hi</mstts:express-as></speak>"#;
        assert_eq!(
            ssml_to_dialect(azure, "elevenlabs-v3").unwrap(),
            "[cheerful] hi"
        );
        // Plain text parses as bare SpeechMarkdown and round-trips
        // unchanged (no translation needed).
        assert_eq!(
            ssml_to_dialect("no ssml here", "elevenlabs").unwrap(),
            "no ssml here"
        );
        // Malformed SSML fails to parse → None → the caller strips.
        assert!(ssml_to_dialect("<speak>a & b</speak>", "elevenlabs").is_none());
        // <voice> has no ElevenLabs equivalent (parity with the old
        // strip path): the modifier is dropped, the text survives.
        let voiced = r#"<speak><voice name="Aria">hi</voice></speak>"#;
        assert_eq!(ssml_to_dialect(voiced, "elevenlabs-v3").unwrap(), "hi");
    }

    #[test]
    pub(crate) fn test_elevenlabs_dialect_follows_extra_body_model_id() {
        // The JSON body lets extra_body override model_id; the SpeechMarkdown
        // dialect must follow the model that is actually sent.
        let mut cfg = build_config("elevenlabs", &engine_creds("elevenlabs")).unwrap();
        assert_eq!(cfg.model_default.as_deref(), Some("eleven_v3"));
        cfg.extra_body.insert(
            "model_id".to_string(),
            serde_json::json!("eleven_multilingual_v2"),
        );
        let effective = effective_model(&cfg);
        assert_eq!(
            elevenlabs_smd_platform("elevenlabs", effective),
            "elevenlabs",
            "pre-v3 override must select the <break> dialect"
        );
    }

    #[test]
    pub(crate) fn test_elevenlabs_dialect_follows_model() {
        // The production predicate used by speak(): eleven_v3* → audio-tag
        // dialect, anything else → pre-v3 <break> markup. Asserted directly
        // against the real helper so a flip fails here, not just live.
        assert_eq!(
            elevenlabs_smd_platform("elevenlabs", Some("eleven_v3")),
            "elevenlabs-v3"
        );
        assert_eq!(
            elevenlabs_smd_platform("elevenlabs", Some("eleven_v3_conversational")),
            "elevenlabs-v3"
        );
        assert_eq!(
            elevenlabs_smd_platform("elevenlabs", Some("eleven_multilingual_v2")),
            "elevenlabs"
        );
        assert_eq!(
            elevenlabs_smd_platform("elevenlabs", Some("eleven_flash_v2")),
            "elevenlabs"
        );
        assert_eq!(elevenlabs_smd_platform("azure", Some("eleven_v3")), "azure");
        assert_eq!(elevenlabs_smd_platform("elevenlabs", None), "elevenlabs");
    }

    // ===== Auth-header composition per provider =====
    //
    // speak() builds the final header value as `format!("{}{}", prefix, api_key)`.
    // Verify the prefix matches each provider's expected scheme, since the
    // the existing inline tests check the config fields but not the joined
    // value the HTTP request actually sends.

    #[test]
    pub(crate) fn test_auth_value_openai_bearer_scheme() {
        let cfg = build_config("openai", &engine_creds("openai")).unwrap();
        let value = format!("{}{}", cfg.auth_prefix, "TESTKEY");
        assert_eq!(value, "Bearer TESTKEY");
    }

    #[test]
    pub(crate) fn test_auth_value_azure_raw_key() {
        // Azure's auth_prefix is empty — the key goes raw under the header.
        let cfg = build_config("azure", &engine_creds("azure")).unwrap();
        let value = format!("{}{}", cfg.auth_prefix, "TESTKEY");
        assert_eq!(value, "TESTKEY");
    }

    #[test]
    pub(crate) fn test_auth_value_deepgram_token_scheme() {
        let cfg = build_config("deepgram", &engine_creds("deepgram")).unwrap();
        let value = format!("{}{}", cfg.auth_prefix, "TESTKEY");
        assert_eq!(value, "Token TESTKEY");
    }

    #[test]
    pub(crate) fn test_auth_value_resemble_token_scheme() {
        let cfg = build_config("resemble", &engine_creds("resemble")).unwrap();
        let value = format!("{}{}", cfg.auth_prefix, "TESTKEY");
        assert_eq!(value, "Token TESTKEY");
    }

    #[test]
    pub(crate) fn test_auth_value_elevenlabs_no_prefix() {
        // xi-api-key carries just the raw key, no prefix.
        let cfg = build_config("elevenlabs", &engine_creds("elevenlabs")).unwrap();
        let value = format!("{}{}", cfg.auth_prefix, "TESTKEY");
        assert_eq!(value, "TESTKEY");
    }

    #[test]
    pub(crate) fn test_auth_value_modelslab_no_header_sent() {
        // ModelsLab puts the key in the body, so auth_header is "". The
        // speak() branch skips header insertion entirely when empty —
        // verify the contract holds.
        let cfg = build_config("modelslab", &engine_creds("modelslab")).unwrap();
        assert_eq!(cfg.auth_header, "");
    }

    // ===== Gemini 3.8 TTS (Interactions API) =====

    #[test]
    pub(crate) fn test_gemini_config_defaults() {
        let cfg = build_config("gemini", &engine_creds("gemini")).unwrap();
        assert_eq!(cfg.provider_id, "gemini");
        assert_eq!(cfg.auth_header, "x-goog-api-key");
        assert_eq!(cfg.auth_prefix, "");
        assert_eq!(cfg.model_default.as_deref(), Some("gemini-3.8-flash-tts"));
        assert_eq!(cfg.default_voice.as_deref(), Some("Kore"));
        assert!(!cfg.body_is_ssml);
        assert!(cfg.voices_url.is_some());
        assert!(cfg.synth_url.contains("/v1beta/interactions"));
    }

    #[test]
    pub(crate) fn test_gemini_config_credential_overrides() {
        let mut creds = engine_creds("gemini");
        creds.insert("modelId".into(), "gemini-3.8-flash-lite-tts".into());
        creds.insert("voice".into(), "Puck".into());
        let cfg = build_config("gemini", &creds).unwrap();
        assert_eq!(
            cfg.model_default.as_deref(),
            Some("gemini-3.8-flash-lite-tts")
        );
        assert_eq!(cfg.default_voice.as_deref(), Some("Puck"));
    }

    #[test]
    pub(crate) fn test_gemini_request_minimal_body() {
        let body = build_gemini_request(
            "Have a wonderful day!",
            "Kore",
            0.0,
            0.0,
            0.0,
            Some("gemini-3.8-flash-tts"),
            None,
        );
        assert_eq!(body["model"], "gemini-3.8-flash-tts");
        assert_eq!(body["response_format"]["type"], "audio");
        assert_eq!(
            body["generation_config"]["speech_config"][0]["voice"],
            "Kore"
        );
        let content = &body["input"][0]["content"][0];
        assert_eq!(content["type"], "text");
        assert_eq!(content["text"], "Have a wonderful day!");
        // No style params set → no annotations (guide: most requests need
        // no style at all).
        assert!(content.get("annotations").is_none());
    }

    #[test]
    pub(crate) fn test_gemini_request_style_from_params() {
        let body = build_gemini_request(
            "Slow down.",
            "Kore",
            0.7, // rate multiplier: slower
            1.2, // pitch multiplier: higher
            0.0,
            None,
            None,
        );
        let content = &body["input"][0]["content"][0];
        let annotations = content["annotations"].as_array().unwrap();
        assert_eq!(annotations[0]["type"], "speech_metadata");
        let style = annotations[0]["style"].as_str().unwrap();
        assert!(style.contains("speaking slowly"), "style: {style}");
        assert!(style.contains("high pitch"), "style: {style}");
    }

    #[test]
    pub(crate) fn test_gemini_request_style_override_wins() {
        let body = build_gemini_request(
            "Hello",
            "Kore",
            0.5,
            0.0,
            0.0,
            None,
            Some("whispered urgently"),
        );
        let style = body["input"][0]["content"][0]["annotations"][0]["style"]
            .as_str()
            .unwrap();
        assert_eq!(style, "whispered urgently");
    }

    #[test]
    pub(crate) fn test_gemini_request_default_model() {
        let body = build_gemini_request("Hi", "Kore", 0.0, 0.0, 0.0, None, None);
        assert_eq!(body["model"], "gemini-3.8-flash-tts");
    }

    #[test]
    pub(crate) fn test_gemini_interaction_audio_parse() {
        let wav = b"RIFFxxxxWAVEfmt";
        let b64 = {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.encode(wav)
        };
        let json = serde_json::json!({
            "steps": [{
                "type": "model_output",
                "content": [
                    { "type": "text", "text": "partial" },
                    { "type": "audio", "mime_type": "audio/wav", "data": b64 }
                ]
            }]
        });
        assert_eq!(
            parse_gemini_interaction_audio(&json),
            GeminiAudioBlock::Present(wav.to_vec())
        );
    }

    #[test]
    pub(crate) fn test_gemini_interaction_audio_corrupt_base64() {
        // An audio block whose payload is not valid base64 must be
        // reported as corrupt (not silently conflated with absence).
        let json = serde_json::json!({
            "steps": [{ "content": [
                { "type": "audio", "mime_type": "audio/wav", "data": "!!!not base64!!!" }
            ]}]
        });
        assert_eq!(
            parse_gemini_interaction_audio(&json),
            GeminiAudioBlock::Corrupt
        );
    }

    #[test]
    pub(crate) fn test_gemini_interaction_audio_none_when_absent() {
        assert_eq!(
            parse_gemini_interaction_audio(&serde_json::json!({})),
            GeminiAudioBlock::Absent
        );
        assert_eq!(
            parse_gemini_interaction_audio(&serde_json::json!({
                "steps": [{ "type": "model_output", "content": [] }]
            })),
            GeminiAudioBlock::Absent
        );
        // An error payload must not panic or yield audio.
        assert_eq!(
            parse_gemini_interaction_audio(&serde_json::json!({
                "error": { "code": 400, "message": "bad" }
            })),
            GeminiAudioBlock::Absent
        );
    }

    #[test]
    pub(crate) fn test_gemini_voices_mapping() {
        let arr = serde_json::json!([
            {
                "id": "kore",
                "display_name": "Kore",
                "language_code": "en-US",
                "accent": "American",
                "persona": "Firm",
                "gender": "female"
            },
            {
                "id": "voice_abc123",
                "display_name": "My Designed Voice",
                "language_code": "de-DE"
            }
        ]);
        let voices = map_gemini_voices(arr.as_array().unwrap());
        assert_eq!(voices.len(), 2);
        assert_eq!(voices[0].id, "kore");
        assert_eq!(voices[0].provider, "gemini");
        assert!(voices[0].name.contains("Kore"));
        assert!(voices[0].name.contains("Firm"), "persona in display name");
        assert_eq!(voices[0].language_codes[0].bcp47, "en-US");
        // No persona → plain display name.
        assert_eq!(voices[1].name, "My Designed Voice");
        assert_eq!(voices[1].language_codes[0].bcp47, "de-DE");
    }

    #[test]
    pub(crate) fn test_gemini_style_params_neutral_is_empty() {
        assert_eq!(gemini_style_from_params(1.0, 1.0, 1.0), "");
        assert_eq!(gemini_style_from_params(0.0, 0.0, 0.0), "");
    }

    #[test]
    pub(crate) fn test_speechmarkdown_gemini_dialect_routing() {
        // The gemini provider must route SpeechMarkdown through the
        // Gemini dialect (angle-bracket tags), NOT SSML.
        let (out, is_ssml) =
            preprocess_speech_markdown("Wait [500ms] then [laugh] loudly", "gemini");
        assert!(!is_ssml, "gemini dialect is prompt text, not SSML");
        assert!(out.contains("<short pause>"), "out: {out}");
        assert!(out.contains("<laugh>"), "out: {out}");
        assert!(!out.contains("<speak>"), "no SSML envelope: {out}");
    }

    #[test]
    pub(crate) fn test_gemini_ssml_input_translated_to_dialect() {
        // tts_speak_ssml on the gemini engine: SSML → SpeechMarkdown →
        // Gemini dialect, not stripped-to-plain.
        #[cfg(feature = "speechmarkdown")]
        {
            let dialect = ssml_to_dialect(
                "<speak>Hello <break time=\"500ms\"/> world</speak>",
                "gemini",
            );
            let out = dialect.expect("SSML → gemini dialect");
            assert!(out.contains("Hello"), "out: {out}");
            assert!(out.contains("<short pause>"), "break preserved: {out}");
        }
    }

    #[test]
    pub(crate) fn test_gemini_engine_id_and_listing() {
        let engine = CloudEngine::new("gemini", &engine_creds("gemini")).unwrap();
        assert_eq!(engine.engine_id(), "gemini");
        let listed = crate::factory::engine_list();
        assert!(listed.iter().any(|e| e.id == "gemini"), "engine listed");
    }

    #[test]
    pub(crate) fn test_gemini_dialect_selector() {
        // The speak()-path SpeechMarkdown selector must resolve the gemini
        // provider to the gemini dialect (regardless of model), or
        // production would silently route to the Alexa SSML default while
        // the preprocess test above stays green.
        assert_eq!(
            elevenlabs_smd_platform("gemini", Some("gemini-3.8-flash-tts")),
            "gemini"
        );
        assert_eq!(
            elevenlabs_smd_platform("gemini", Some("gemini-3.8-flash-lite-tts")),
            "gemini"
        );
    }

    /// Build a minimal valid mono 16-bit WAV buffer.
    pub(crate) fn tiny_wav(samples: &[i16], sample_rate: u32) -> Vec<u8> {
        let data_len = samples.len() * 2;
        let mut wav = Vec::with_capacity(44 + data_len);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&1u16.to_le_bytes()); // mono
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        wav.extend_from_slice(&(sample_rate * 2).to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data_len as u32).to_le_bytes());
        for s in samples {
            wav.extend_from_slice(&s.to_le_bytes());
        }
        wav
    }

    #[test]
    pub(crate) fn test_gemini_wav_decode_roundtrip() {
        // Real decode path: a valid WAV fixture must survive
        // symphonia → PCM16 (regression guard for the wav/pcm features).
        let samples: Vec<i16> = (0..480).map(|i| (i * 37 % 3000) as i16).collect();
        let wav = tiny_wav(&samples, 24_000);
        let pcm = decode_audio_to_pcm16_mono(&wav, "wav");
        assert_eq!(pcm.len(), samples.len() * 2, "16-bit mono out");
        for (i, s) in samples.iter().enumerate() {
            let out = i16::from_le_bytes([pcm[i * 2], pcm[i * 2 + 1]]);
            assert!((out - s).abs() <= 2, "sample {i}: {out} vs {s}");
        }
    }

    #[test]
    pub(crate) fn test_wav_sample_rate_parsing() {
        let samples = [0i16; 16];
        assert_eq!(wav_sample_rate(&tiny_wav(&samples, 24_000)), 24_000);
        assert_eq!(wav_sample_rate(&tiny_wav(&samples, 44_100)), 44_100);
        // Non-WAV garbage falls back to the documented 24 kHz default.
        assert_eq!(wav_sample_rate(b"not a wav file at all......"), 24_000);
        assert_eq!(wav_sample_rate(&[]), 24_000);
    }

    type BoundaryEvent = (String, f32, f32, i32, i32, bool);

    #[test]
    pub(crate) fn test_fire_scaled_estimates_scales_to_duration() {
        // 2 s of silence at 24 kHz. The 150-wpm estimator will produce
        // events for the words; scaling must stretch them to the real
        // duration, and every event is flagged estimated=true.
        let pcm = vec![0u8; 2 * 24_000 * 2];
        let mut events: Vec<BoundaryEvent> = Vec::new();
        {
            let mut cb: crate::engine::OnBoundaryCallback<'_> =
                &mut |word: &str, start: f32, end: f32, offset: i32, len: i32, est: bool| {
                    events.push((word.to_string(), start, end, offset, len, est));
                };
            fire_scaled_estimates(&mut cb, "one two three four five six seven", &pcm, 24_000);
        }
        assert!(!events.is_empty(), "events fired");
        let last_end = events.last().unwrap().2;
        assert!(
            (last_end - 2.0).abs() < 0.2,
            "last end {last_end} scaled to ~2.0 s of audio"
        );
        assert!(events.iter().all(|e| e.5), "estimated flag set");
        assert!(
            events.iter().all(|e| e.0.split(' ').count() == 1),
            "one word per event"
        );
    }

    #[test]
    pub(crate) fn test_fire_scaled_estimates_empty_pcm() {
        // No audio: must not panic, must not fire.
        let mut fired = 0;
        let mut cb: crate::engine::OnBoundaryCallback<'_> =
            &mut |_w: &str, _s: f32, _e: f32, _o: i32, _l: i32, _est: bool| {
                fired += 1;
            };
        fire_scaled_estimates(&mut cb, "some words here", &[], 24_000);
        assert_eq!(fired, 0, "zero-length audio fires nothing (all at t=0)");
    }
}
