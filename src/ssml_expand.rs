//! Wrapper-level `<say-as>` expansion: interpret `characters`, `cardinal`,
//! `ordinal`, `date`, `time`, `currency` and `telephone` into spoken words
//! for engines without native SSML semantics (pocket, qwen3-local).
//!
//! Engines that speak SSML natively (Azure, Polly, Google) receive the tags
//! verbatim from [`crate::engine::preprocess_speech_markdown`] and never
//! see this module.
//!
//! Each source word expands to one or more spoken words; every expanded
//! word carries the character span of its source so boundary events can
//! highlight what the user typed, not what was spoken.

use std::ops::Range;

use floravox_ssml::{DateStyle, SayAs, TimeStyle};

/// One spoken word produced by say-as interpretation.
#[derive(Debug, Clone, PartialEq)]
pub struct ExpandedWord {
    /// The spoken form (already interpreted; feed to g2p / the engine).
    pub spoken: String,
    /// Char range of the source text this word was expanded from.
    pub char_span: Range<usize>,
}

/// Expand `text` (a word as written) according to `say_as`. Words with no
/// special interpretation (or unparseable values) pass through unchanged.
#[must_use]
pub fn expand_word(text: &str, char_span: Range<usize>, say_as: SayAs) -> Vec<ExpandedWord> {
    fn one(spoken: String, char_span: &Range<usize>) -> Vec<ExpandedWord> {
        vec![ExpandedWord {
            spoken,
            char_span: char_span.clone(),
        }]
    }
    fn non_empty(words: Vec<ExpandedWord>) -> Option<Vec<ExpandedWord>> {
        (!words.is_empty()).then_some(words)
    }

    let expanded = match say_as {
        SayAs::None | SayAs::Other | SayAs::Phonemes { .. } => None,
        SayAs::Characters => non_empty(characters(text, char_span.clone())),
        SayAs::Cardinal => cardinal(text).map(|w| one(w, &char_span)),
        SayAs::Ordinal => ordinal(text).map(|w| one(w, &char_span)),
        SayAs::Date { format } => date(text, format, &char_span),
        SayAs::Time { format } => time(text, format, &char_span),
        SayAs::Currency => currency(text, &char_span),
        SayAs::Telephone => non_empty(telephone(text, char_span.clone())),
    };
    expanded.unwrap_or_else(|| one(text.to_string(), &char_span))
}

const ONES: [&str; 20] = [
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
];
const TENS: [&str; 10] = [
    "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
];
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const DIGIT_WORDS: [&str; 10] = [
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
];

fn triple_to_words(n: u32, out: &mut Vec<String>) {
    let hundreds = n / 100;
    let rest = n % 100;
    if hundreds > 0 {
        out.push(ONES[hundreds as usize].to_string());
        out.push("hundred".to_string());
    }
    if rest >= 20 {
        out.push(TENS[(rest / 10) as usize].to_string());
        if !rest.is_multiple_of(10) {
            out.push(ONES[(rest % 10) as usize].to_string());
        }
    } else if rest > 0 {
        out.push(ONES[rest as usize].to_string());
    }
}

/// Cardinal number to English words (0 ..= 999,999,999,999). Returns
/// `None` for unparseable input. Zero maps to `"zero"`.
fn number_words(s: &str) -> Option<String> {
    let cleaned: String = s
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '-' || *c == ',')
        .collect();
    let negative = cleaned.starts_with('-');
    let digits: String = cleaned.chars().filter(char::is_ascii_digit).collect();
    if digits.is_empty() || digits.len() > 12 {
        return None;
    }
    let n: u128 = digits.parse().ok()?;
    let mut out: Vec<String> = Vec::new();
    let scales = [
        (1_000_000_000u128, "billion"),
        (1_000_000, "million"),
        (1_000, "thousand"),
    ];
    let mut rest = n;
    for (scale, name) in scales {
        if rest >= scale {
            let group = (rest / scale) as u32;
            triple_to_words(group, &mut out);
            out.push(name.to_string());
            rest %= scale;
        }
    }
    if rest > 0 || out.is_empty() {
        triple_to_words(rest as u32, &mut out);
    }
    let mut words = out.join(" ");
    if negative {
        words = format!("minus {words}");
    }
    Some(words)
}

fn ordinal_words_from_number(n: u128) -> Option<String> {
    if n >= 100_000_000 {
        // very large ordinals read as "<cardinal>th" is wrong; fall back
        // to the cardinal reading
        return None;
    }
    let mod100 = n % 100;
    let special = match n % 100 {
        11..=13 => "th",
        _ => match mod100 % 10 {
            1 => "st",
            2 => "nd",
            3 => "rd",
            _ => "th",
        },
    };
    let _ = special;
    // build words for n, then convert the final word to its ordinal form
    let mut words: Vec<String> = Vec::new();
    let mut rest = n;
    let scales = [(1_000_000u128, "million"), (1_000, "thousand")];
    for (scale, name) in scales {
        if rest >= scale {
            triple_to_words((rest / scale) as u32, &mut words);
            if rest.is_multiple_of(scale) {
                // "three thousand" -> "three-thousandth" spoken as
                // "three thousandth"
                words.push(format!("{name}th"));
                return Some(words.join(" "));
            }
            words.push(name.to_string());
            rest %= scale;
        }
    }
    triple_to_words(rest as u32, &mut words);
    if words.is_empty() {
        return Some("zeroth".to_string());
    }
    let last = words.pop().unwrap_or_default();
    let ord = match last.as_str() {
        "one" => "first".to_string(),
        "two" => "second".to_string(),
        "three" => "third".to_string(),
        "five" => "fifth".to_string(),
        "eight" => "eighth".to_string(),
        "nine" => "ninth".to_string(),
        "twelve" => "twelfth".to_string(),
        w if w.ends_with('y') => format!("{}ieth", &w[..w.len() - 1]),
        w => format!("{w}th"),
    };
    words.push(ord);
    Some(words.join(" "))
}

fn cardinal(text: &str) -> Option<String> {
    number_words(text)
}

/// "3", "3rd", "21st" -> "third", "third", "twenty-first"
fn ordinal(text: &str) -> Option<String> {
    let cleaned = text.trim();
    // strip an existing ordinal suffix
    let digits = cleaned
        .strip_suffix("st")
        .or_else(|| cleaned.strip_suffix("nd"))
        .or_else(|| cleaned.strip_suffix("rd"))
        .or_else(|| cleaned.strip_suffix("th"))
        .map_or(cleaned, str::trim);
    ordinal_words_from_number(digits.parse().ok()?)
}

/// Spell out character by character. Digits become digit names; letters
/// stay as single letters (the g2p reads letter names); other characters
/// are dropped.
fn characters(text: &str, char_span: Range<usize>) -> Vec<ExpandedWord> {
    let mut out = Vec::new();
    let mut char_offset = 0usize;
    for c in text.chars() {
        let span_start = char_span.start + char_offset;
        char_offset += 1;
        let span = span_start..(char_span.start + char_offset);
        if let Some(d) = c.to_digit(10) {
            out.push(ExpandedWord {
                spoken: DIGIT_WORDS[d as usize].to_string(),
                char_span: span,
            });
        } else if c.is_alphabetic() {
            out.push(ExpandedWord {
                spoken: c.to_uppercase().to_string(),
                char_span: span,
            });
        }
    }
    out
}

/// Digits spoken one at a time ("555-867" -> five five five eight six seven)
fn telephone(text: &str, char_span: Range<usize>) -> Vec<ExpandedWord> {
    let mut out = Vec::new();
    let mut char_offset = 0usize;
    for c in text.chars() {
        let span_start = char_span.start + char_offset;
        char_offset += 1;
        let span = span_start..(char_span.start + char_offset);
        if let Some(d) = c.to_digit(10) {
            out.push(ExpandedWord {
                spoken: DIGIT_WORDS[d as usize].to_string(),
                char_span: span,
            });
        } else if c == '+' {
            out.push(ExpandedWord {
                spoken: "plus".to_string(),
                char_span: span,
            });
        }
    }
    out
}

/// Dates: "10/12/2026", "2026-10-12", "12.10.2026" with mdy/dmy/ymd hints.
fn date(text: &str, format: DateStyle, char_span: &Range<usize>) -> Option<Vec<ExpandedWord>> {
    let cleaned: String = text
        .chars()
        .filter(|c| c.is_ascii_digit() || matches!(c, '/' | '-' | '.'))
        .collect();
    let parts: Vec<&str> = cleaned.split(['/', '-', '.']).collect();
    if parts.len() != 3 {
        return None;
    }
    let nums: Vec<u32> = parts
        .iter()
        .map(|p| p.parse().ok())
        .collect::<Option<Vec<_>>>()?;

    let style = match format {
        DateStyle::Auto => {
            if parts[0].len() == 4 {
                DateStyle::YearMonthDay
            } else if nums[0] > 12 && nums[0] <= 31 {
                DateStyle::DayMonthYear
            } else {
                DateStyle::MonthDayYear
            }
        }
        s => s,
    };

    let (y, m, d) = match style {
        DateStyle::YearMonthDay => (nums[0], nums[1], nums[2]),
        DateStyle::MonthDayYear => (nums[2], nums[0], nums[1]),
        DateStyle::DayMonthYear => (nums[2], nums[1], nums[0]),
        DateStyle::Auto => unreachable!(),
    };
    if !(1..=12).contains(&m) || y == 0 {
        return None;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let days_in_month = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        _ => 0,
    };
    if d == 0 || d > days_in_month {
        return None;
    }

    let mut out = Vec::new();
    let span = char_span.clone();
    if matches!(style, DateStyle::YearMonthDay) {
        out.push(ExpandedWord {
            spoken: number_words(&y.to_string())?,
            char_span: span.clone(),
        });
    }
    out.push(ExpandedWord {
        spoken: MONTHS[(m - 1) as usize].to_string(),
        char_span: span.clone(),
    });
    out.push(ExpandedWord {
        spoken: ordinal_words_from_number(u128::from(d))?,
        char_span: span.clone(),
    });
    if !matches!(style, DateStyle::YearMonthDay) {
        out.push(ExpandedWord {
            spoken: number_words(&y.to_string())?,
            char_span: span,
        });
    }
    Some(out)
}

/// Times: "2:30", "14:30", "2:30pm". hms12/hms24 hint the clock.
fn time(text: &str, format: TimeStyle, char_span: &Range<usize>) -> Option<Vec<ExpandedWord>> {
    let lower = text.trim().to_ascii_lowercase();
    let (core, meridiem) = match (lower.strip_suffix("am"), lower.strip_suffix("pm")) {
        (Some(c), _) => (c.trim(), Some("AM")),
        (_, Some(c)) => (c.trim(), Some("PM")),
        _ => (lower.trim(), None),
    };
    let mut it = core.splitn(2, ':');
    let hour: u32 = it.next()?.parse().ok()?;
    let minute: u32 = it.next().unwrap_or("0").parse().ok()?;
    if hour > 23 || minute > 59 {
        return None;
    }

    let span = char_span.clone();
    let mut out = Vec::new();
    let h12 = if hour == 0 {
        12
    } else if hour > 12 {
        hour - 12
    } else {
        hour
    };

    let use_12 = match format {
        TimeStyle::TwelveHour => true,
        TimeStyle::TwentyFourHour => false,
        TimeStyle::Auto => hour <= 12 || meridiem.is_some(),
    };
    if use_12 {
        out.push(ExpandedWord {
            spoken: ONES[h12 as usize].to_string(),
            char_span: span.clone(),
        });
        if minute == 0 {
            out.push(ExpandedWord {
                spoken: "o'clock".to_string(),
                char_span: span.clone(),
            });
        } else if minute < 10 {
            out.push(ExpandedWord {
                spoken: format!("oh {}", ONES[minute as usize]),
                char_span: span.clone(),
            });
        } else {
            out.push(ExpandedWord {
                spoken: number_words(&minute.to_string())?,
                char_span: span.clone(),
            });
        }
        if let Some(m) = meridiem {
            out.push(ExpandedWord {
                spoken: m.to_string(),
                char_span: span.clone(),
            });
        }
    } else {
        out.push(ExpandedWord {
            spoken: number_words(&hour.to_string())?,
            char_span: span.clone(),
        });
        if minute > 0 {
            out.push(ExpandedWord {
                spoken: number_words(&minute.to_string())?,
                char_span: span.clone(),
            });
        } else {
            out.push(ExpandedWord {
                spoken: "hundred".to_string(),
                char_span: span.clone(),
            });
        }
    }
    Some(out)
}

/// Currency: "$5.50", "£3", "€7.20", "¥1000", "USD 9.99".
fn currency(text: &str, char_span: &Range<usize>) -> Option<Vec<ExpandedWord>> {
    let t = text.trim();
    let (major_word, rest) = if let Some(r) = t.strip_prefix('$') {
        ("dollar", r)
    } else if let Some(r) = t.strip_prefix('€') {
        ("euro", r)
    } else if let Some(r) = t.strip_prefix('£') {
        ("pound", r)
    } else if let Some(r) = t.strip_prefix('¥') {
        ("yen", r)
    } else if let Some(r) = t.strip_prefix('₹') {
        ("rupee", r)
    } else if let Some(r) = t.strip_prefix("USD ") {
        ("dollar", r)
    } else if let Some(r) = t.strip_prefix("EUR ") {
        ("euro", r)
    } else {
        let r = t.strip_prefix("GBP ")?;
        ("pound", r)
    };

    let rest = rest.trim();
    let (int_part, frac_part) = match rest.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (rest, None),
    };
    // yen has no minor unit in common speech
    let has_minor = major_word != "yen";

    let int_val: u128 = if int_part.is_empty() {
        0
    } else {
        int_part.parse().ok()?
    };
    let span = char_span.clone();
    let mut out = Vec::new();
    if int_val > 0 || frac_part.is_none() {
        out.push(ExpandedWord {
            spoken: number_words(int_part)?,
            char_span: span.clone(),
        });
        let plural = int_val != 1 && major_word != "yen";
        out.push(ExpandedWord {
            spoken: if plural {
                format!("{major_word}s")
            } else {
                major_word.to_string()
            },
            char_span: span.clone(),
        });
    }
    if let Some(f) = frac_part.filter(|f| !f.is_empty() && has_minor) {
        if f.len() <= 2 {
            let cents: u128 = if f.len() == 1 {
                format!("{f}0").parse().ok()?
            } else {
                f.parse().ok()?
            };
            if cents > 0 {
                out.push(ExpandedWord {
                    spoken: number_words(&cents.to_string())?,
                    char_span: span.clone(),
                });
                out.push(ExpandedWord {
                    spoken: if cents == 1 {
                        "cent".to_string()
                    } else {
                        "cents".to_string()
                    },
                    char_span: span.clone(),
                });
            }
        }
    }
    if out.is_empty() {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str, say_as: SayAs) -> Vec<String> {
        expand_word(text, 0..text.chars().count(), say_as)
            .into_iter()
            .map(|w| w.spoken)
            .collect()
    }

    #[test]
    fn cardinal_expansion() {
        assert_eq!(
            words("1234", SayAs::Cardinal),
            vec!["one thousand two hundred thirty four"]
        );
        assert_eq!(words("42", SayAs::Cardinal), vec!["forty two"]);
        assert_eq!(words("1,000,000", SayAs::Cardinal), vec!["one million"]);
    }

    #[test]
    fn ordinal_expansion() {
        assert_eq!(words("3", SayAs::Ordinal), vec!["third"]);
        assert_eq!(words("21st", SayAs::Ordinal), vec!["twenty first"]);
        assert_eq!(words("112", SayAs::Ordinal), vec!["one hundred twelfth"]);
    }

    #[test]
    fn characters_expansion() {
        assert_eq!(words("abc", SayAs::Characters), vec!["A", "B", "C"]);
        assert_eq!(words("a5c", SayAs::Characters), vec!["A", "five", "C"]);
    }

    #[test]
    fn date_expansion() {
        use floravox_ssml::DateStyle::*;
        assert_eq!(
            words(
                "10/12/2026",
                SayAs::Date {
                    format: MonthDayYear
                }
            ),
            vec!["October", "twelfth", "two thousand twenty six"]
        );
        assert_eq!(
            words("2026-10-12", SayAs::Date { format: Auto }),
            vec!["two thousand twenty six", "October", "twelfth"]
        );
        // auto: first field > 12 must be a day
        assert_eq!(
            words("25/12/2026", SayAs::Date { format: Auto }),
            vec!["December", "twenty fifth", "two thousand twenty six"]
        );
    }

    #[test]
    fn time_expansion() {
        use floravox_ssml::TimeStyle::*;
        assert_eq!(
            words("2:30", SayAs::Time { format: Auto }),
            vec!["two", "thirty"]
        );
        assert_eq!(
            words(
                "14:30",
                SayAs::Time {
                    format: TwentyFourHour
                }
            ),
            vec!["fourteen", "thirty"]
        );
        assert_eq!(
            words("2:00pm", SayAs::Time { format: Auto }),
            vec!["two", "o'clock", "PM"]
        );
        assert_eq!(
            words("9:05", SayAs::Time { format: TwelveHour }),
            vec!["nine", "oh five"]
        );
    }

    #[test]
    fn currency_expansion() {
        assert_eq!(
            words("$5.50", SayAs::Currency),
            vec!["five", "dollars", "fifty", "cents"]
        );
        assert_eq!(words("£3", SayAs::Currency), vec!["three", "pounds"]);
        assert_eq!(words("¥1000", SayAs::Currency), vec!["one thousand", "yen"]);
        assert_eq!(words("$1", SayAs::Currency), vec!["one", "dollar"]);
    }

    #[test]
    fn telephone_expansion() {
        assert_eq!(
            words("555-867-5309", SayAs::Telephone)[..4],
            vec!["five", "five", "five", "eight"]
        );
    }

    #[test]
    fn passthrough_on_garbage() {
        assert_eq!(words("hello", SayAs::Cardinal), vec!["hello"]);
        assert_eq!(
            words(
                "31/02/2026",
                SayAs::Date {
                    format: floravox_ssml::DateStyle::Auto
                }
            )
            .len(),
            1
        );
    }

    #[test]
    fn expanded_words_carry_spans() {
        let out = expand_word("a5c", 7..10, SayAs::Characters);
        assert_eq!(out[0].char_span, 7..8);
        assert_eq!(out[2].char_span, 9..10);
    }
}

/// Parse W3C SSML and return the plain spoken text with every
/// `<say-as>` interpreted, `<sub alias>` substituted, `<phoneme>` tags
/// contributing their display text, and tags otherwise dropped.
///
/// Engines without SSML support (qwen3-local and friends) call this
/// instead of a tag-stripper so `(5)[cardinal]` and friends still read
/// correctly. Returns `None` when the input is not SSML.
#[must_use]
pub fn expand_ssml_to_text(ssml: &str) -> Option<String> {
    let doc = floravox_ssml::parse(ssml).ok()?;
    let mut out: Vec<String> = Vec::new();
    for w in doc.words() {
        let base = if w.spoken.is_empty() {
            w.text.clone()
        } else {
            w.spoken.clone()
        };
        if w.phonemes.as_deref().unwrap_or_default().is_empty()
            && !matches!(
                w.say_as,
                floravox_ssml::SayAs::None | floravox_ssml::SayAs::Other
            )
        {
            for ex in expand_word(&base, w.char_span.clone(), w.say_as) {
                out.push(ex.spoken);
            }
        } else {
            out.push(base);
        }
    }
    Some(out.join(" "))
}

#[cfg(test)]
mod roundtrip_tests {
    use super::*;

    /// SpeechMarkdown say-as forms must survive the full
    /// markdown -> SSML -> expansion pipeline.
    #[test]
    #[cfg(feature = "speechmarkdown")]
    fn speechmarkdown_sayas_roundtrip() {
        // characters
        let (ssml, is_ssml) =
            crate::engine::preprocess_speech_markdown("(abc)[characters]", "plain");
        assert!(is_ssml, "markdown should convert: {ssml}");
        let text = expand_ssml_to_text(&ssml).unwrap_or_default();
        assert_eq!(text, "A B C", "from {ssml}");

        // cardinal
        let (ssml, _) = crate::engine::preprocess_speech_markdown("(1234)[number]", "plain");
        let text = expand_ssml_to_text(&ssml).unwrap_or_default();
        assert_eq!(text, "one thousand two hundred thirty four", "from {ssml}");

        // telephone (SpeechMarkdown's [tel] shortcut)
        let (ssml, _) = crate::engine::preprocess_speech_markdown("(555)[telephone]", "plain");
        let text = expand_ssml_to_text(&ssml).unwrap_or_default();
        assert_eq!(text, "five five five", "from {ssml}");
    }

    /// Engine-native SSML passes through expansion with say-as applied.
    #[test]
    fn raw_ssml_sayas_expansion() {
        let text = expand_ssml_to_text(
            "<speak>Room <say-as interpret-as=\"cardinal\">101</say-as> costs \
             <say-as interpret-as=\"currency\">$5.50</say-as></speak>",
        )
        .unwrap_or_default();
        assert_eq!(text, "Room one hundred one costs five dollars fifty cents");
    }

    /// `<phoneme>` keeps its display text for non-phoneme engines.
    #[test]
    fn phoneme_display_text_survives() {
        let text = expand_ssml_to_text(
            "<speak>Hello <phoneme alphabet=\"ipa\" ph=\"wɜːld\">world</phoneme></speak>",
        )
        .unwrap_or_default();
        assert_eq!(text, "Hello world");
    }

    /// `<sub alias>` substitution flows through.
    #[test]
    fn sub_alias_flows() {
        let text = expand_ssml_to_text(
            "<speak><sub alias=\"artificial intelligence\">AI</sub> speaks</speak>",
        )
        .unwrap_or_default();
        assert_eq!(text, "artificial intelligence speaks");
    }
}

/// One text segment between SSML `<break>` tags, with the pause that
/// follows it (milliseconds). The final segment has no trailing pause.
#[derive(Debug, Clone, PartialEq)]
pub struct SsmlSegment {
    /// Plain spoken text for this segment (say-as expanded, sub aliased)
    pub text: String,
    /// Pause after this segment in milliseconds (from `<break time="X"/>`)
    pub pause_after_ms: u32,
    /// Mark names that fire at the START of this segment
    pub marks: Vec<String>,
}

/// Parse SSML into segments split at `<break>` boundaries, preserving
/// mark positions. Returns `None` when the input is not SSML.
///
/// This is the structured counterpart to [`expand_ssml_to_text`]: engines
/// that can insert silence gaps (qwen3-local, omnivoice-local) use this
/// to synthesise each segment independently and concatenate with pauses.
#[must_use]
pub fn expand_ssml_to_segments(ssml: &str) -> Option<Vec<SsmlSegment>> {
    let doc = floravox_ssml::parse(ssml).ok()?;

    let mut segments: Vec<SsmlSegment> = vec![SsmlSegment {
        text: String::new(),
        pause_after_ms: 0,
        marks: Vec::new(),
    }];
    let mut pending_marks: Vec<String> = Vec::new();

    for seg in &doc.segments {
        match seg {
            floravox_ssml::Segment::Words { words } => {
                for w in words {
                    let base = if w.spoken.is_empty() {
                        w.text.clone()
                    } else {
                        w.spoken.clone()
                    };
                    // say-as expansion
                    if w.phonemes.as_deref().unwrap_or_default().is_empty()
                        && !matches!(
                            w.say_as,
                            floravox_ssml::SayAs::None | floravox_ssml::SayAs::Other
                        )
                    {
                        for ex in expand_word(&base, w.char_span.clone(), w.say_as) {
                            let last = segments.last_mut()?;
                            if !last.text.is_empty() {
                                last.text.push(' ');
                            }
                            last.text.push_str(&ex.spoken);
                        }
                    } else {
                        let last = segments.last_mut()?;
                        if !last.text.is_empty() {
                            last.text.push(' ');
                        }
                        last.text.push_str(&base);
                    }
                }
            }
            floravox_ssml::Segment::Break { ms, .. } => {
                // close the current segment, start a new one
                if let Some(last) = segments.last_mut() {
                    last.pause_after_ms = *ms;
                }
                segments.push(SsmlSegment {
                    text: String::new(),
                    pause_after_ms: 0,
                    marks: Vec::new(),
                });
            }
            floravox_ssml::Segment::Mark { name, .. } => {
                pending_marks.push(name.clone());
            }
            _ => {}
        }
    }

    // attach pending marks to the current (last) segment
    if !pending_marks.is_empty() {
        if let Some(last) = segments.last_mut() {
            last.marks = pending_marks;
        }
    }

    // drop empty segments that have no marks and no pause
    segments.retain(|s| !s.text.is_empty() || !s.marks.is_empty() || s.pause_after_ms > 0);

    Some(segments)
}
