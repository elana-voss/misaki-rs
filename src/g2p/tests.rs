use super::*;
use super::links::parse_link_feature;

struct TestFallback;

impl Fallback for TestFallback {
    fn phonemize(&self, word: &str) -> Result<String, FallbackError> {
        assert_eq!(word, "qzxwv");
        Ok("test-phonemes".to_string())
    }
}

#[test]
fn test_g2p_basic() {
    let _ = env_logger::try_init();
    let g2p = G2P::new(Language::EnglishUS);
    let (phonemes, _) = g2p.g2p("Hello, world!").unwrap();
    println!("Phonemes: {}", phonemes);
    assert!(!phonemes.contains("❓"));
}

#[test]
fn test_with_fallback_uses_injected_fallback_for_oov_word() {
    let g2p = G2P::with_fallback(Language::EnglishUS, Some(Box::new(TestFallback)));

    let (phonemes, tokens) = g2p.g2p("qzxwv").unwrap();

    assert_eq!(phonemes.trim(), "test-phonemes");
    assert_eq!(tokens[0].phonemes.as_deref(), Some("test-phonemes"));
}

// Tests for `[text](feature)` markdown-link parsing — see
// misaki/en.py::Lexicon.preprocess for the reference implementation.

#[test]
fn test_preprocess_links_strips_markup() {
    let g2p = G2P::new(Language::EnglishUS);
    let (stripped, markers) = g2p.preprocess_links("say [hello](+2) please");
    assert_eq!(stripped, "say hello please");
    assert_eq!(markers.len(), 1);
    let m = &markers[0];
    assert_eq!(&stripped[m.byte_start..m.byte_end], "hello");
    match m.feature {
        LinkFeature::Stress(n) => assert!((n - 2.0).abs() < f64::EPSILON),
        _ => panic!("expected Stress, got {:?}", m.feature),
    }
}

#[test]
fn test_preprocess_links_parses_features() {
    // Integer stress — signed and unsigned.
    assert!(matches!(parse_link_feature("+2"), Some(LinkFeature::Stress(n)) if n == 2.0));
    assert!(matches!(parse_link_feature("-1"), Some(LinkFeature::Stress(n)) if n == -1.0));
    assert!(matches!(parse_link_feature("2"),  Some(LinkFeature::Stress(n)) if n == 2.0));
    // Half stress — only these three literals are accepted upstream.
    assert!(matches!(parse_link_feature("0.5"),  Some(LinkFeature::Stress(n)) if n == 0.5));
    assert!(matches!(parse_link_feature("+0.5"), Some(LinkFeature::Stress(n)) if n == 0.5));
    assert!(matches!(parse_link_feature("-0.5"), Some(LinkFeature::Stress(n)) if n == -0.5));
    // Phoneme override.
    assert!(matches!(parse_link_feature("/həˈloʊ/"), Some(LinkFeature::Phonemes(ref p)) if p == "həˈloʊ"));
    // Num flags.
    assert!(matches!(parse_link_feature("#cardinal#"), Some(LinkFeature::NumFlags(ref f)) if f == "cardinal"));
    // Anything else → None, matching the upstream `else: f = None` branch.
    assert!(parse_link_feature("").is_none());
    assert!(parse_link_feature("=").is_none());
    assert!(parse_link_feature("doctor").is_none());
    assert!(parse_link_feature("2.5").is_none());
    assert!(parse_link_feature("/foo").is_none());
    assert!(parse_link_feature("foo/").is_none());
}

#[test]
fn test_link_stress_promotes() {
    // `[hello](+2)` should phonemize successfully and emit primary stress.
    let g2p = G2P::new(Language::EnglishUS);
    let (boosted, _) = g2p.g2p("[hello](+2)").unwrap();
    assert!(!boosted.contains("❓"), "boosted output: '{}'", boosted);
    assert!(boosted.contains('ˈ'),
        "boosted output missing primary stress: '{}'", boosted);
}

#[test]
fn test_link_stress_strips() {
    // `(-2)` strips all stress marks via Lexicon::apply_stress.
    let g2p = G2P::new(Language::EnglishUS);
    let (stripped, _) = g2p.g2p("[hello](-2)").unwrap();
    assert!(!stripped.contains("❓"));
    assert!(!stripped.contains('ˈ') && !stripped.contains('ˌ'),
        "expected no stress marks, got: '{}'", stripped);
}

#[test]
fn test_link_phoneme_override() {
    // `/.../` content is emitted verbatim, bypassing the lexicon.
    let g2p = G2P::new(Language::EnglishUS);
    let (out, _) = g2p.g2p("[xyzzy](/həˈloʊ/)").unwrap();
    assert!(out.contains("həˈloʊ"), "phoneme override missing: '{}'", out);
    assert!(!out.contains("❓"), "got unknown marker: '{}'", out);
}

#[test]
fn test_link_num_flags_sets_field() {
    // `#flag#` markers don't affect phonemes today (the number-spelling
    // logic that reads `num_flags` isn't ported yet), but the field must
    // be set on the token so a future port can consume it.
    let g2p = G2P::new(Language::EnglishUS);
    let (_, tokens) = g2p.g2p("[5](#cardinal#)").unwrap();
    let tagged: Vec<_> = tokens
        .iter()
        .filter(|t| !t.underscore().num_flags.is_empty())
        .collect();
    assert_eq!(tagged.len(), 1, "expected one token with num_flags set");
    assert_eq!(tagged[0].underscore().num_flags, "cardinal");
}

#[test]
fn test_link_unknown_feature_is_dropped() {
    // `=` and bare words must not parse — they fall through to None and
    // the marker is silently ignored, matching upstream behaviour.
    let g2p = G2P::new(Language::EnglishUS);
    let (stripped, markers) = g2p.preprocess_links("say [hello](=) please");
    assert_eq!(stripped, "say hello please");
    assert!(markers.is_empty(), "unknown feature should yield no marker");
}

/// With espeak feature: "eBook" is phonemized by espeak fallback (not in lexicon).
#[test]
#[cfg(feature = "espeak")]
fn test_ebook_with_espeak() {
    let g2p = G2P::new(Language::EnglishUS);
    let (phonemes, _) = g2p.g2p("eBook").unwrap();
    assert!(
        !phonemes.contains("❓"),
        "with espeak: 'eBook' should be phonemized by fallback, got: {}",
        phonemes
    );
    // Should not be spelled out letter-by-letter (e.g. no ˈi for 'e' as letter)
    assert!(
        !phonemes.contains("ˈɛl"),
        "with espeak: 'eBook' should not spell out letters, got: {}",
        phonemes
    );
    println!("eBook (with espeak): {}", phonemes);
}

/// Without espeak feature: "eBook" is OOV so we get unknown marker or character spelling.
#[test]
#[cfg(not(feature = "espeak"))]
fn test_ebook_without_espeak() {
    let g2p = G2P::new(Language::EnglishUS);
    let (phonemes, _) = g2p.g2p("eBook").unwrap();
    // No fallback: either ❓ for unknown or character-by-character spelling
    assert!(
        phonemes.contains("❓") || phonemes.contains("ˈi") || phonemes.contains("b"),
        "without espeak: 'eBook' should show unknown or spelled form, got: {}",
        phonemes
    );
    println!("eBook (without espeak): {}", phonemes);
}

// #[test]
// fn test_g2p_italian() {
//     let g2p = G2P::new(Language::Italian);
//     let (phonemes, _) = g2p.g2p("Ciao, mondo!");
//     println!("Phonemes: {}", phonemes);
//     // "ciao" -> c+i+a+o -> tʃ+a+o -> with stress tʃˈao
//     // "mondo" -> m+o+n+d+o -> mˈondo
//     assert!(phonemes.contains("tʃ") && phonemes.contains("ao"));
//     assert!(phonemes.contains("mondo"));
// }

// #[test]
// fn test_convert_number_italian() {
//     let g2p = G2P::new(Language::Italian);
//     let (phonemes, _) = g2p.g2p("42");
//     println!("Phonemes for 42: {}", phonemes);
//     // 42 in Italian is "quarantadue" -> kwarantadue
//     // We relax the check to ensure it produces phonemes and not numbers/unknowns
//     assert!(!phonemes.contains("42"));
//     assert!(!phonemes.contains("❓"));
//     assert!(phonemes.contains("kwaranta") || phonemes.contains("due"));
// }

#[test]
fn test_english_abbreviations() {
    let g2p = G2P::new(Language::EnglishUS);
    let cases = vec![
        "I'll",
        "I've",
        "it's",
        "he's",
        "she's",
        "we're",
        "they're",
        "isn't",
        "aren't",
        "wasn't",
        "weren't",
        "don't",
        "doesn't",
        "didn't",
        "can't",
        "couldn't",
        "shouldn't",
        "wouldn't",
        "won't",
        "hasn't",
        "haven't",
        "hadn't",
        "let's",
        "that's",
        "what's",
        "who's",
        "here's",
        "there's",
        "where's",
        "how's",
    ];
    for text in cases {
        let (p, _) = g2p.g2p(text).unwrap();
        println!("'{}' -> '{}'", text, p);
        assert!(!p.contains("❓"), "Failed for '{}'", text);
    }
}

#[test]
fn test_casing_and_special_chars() {
    let g2p = G2P::new(Language::EnglishUS);

    // Test 1: All caps with suffix
    let (playing, _) = g2p.g2p("PLAYING").unwrap();
    println!("PLAYING: {}", playing);
    assert!(
        !playing.contains("❓"),
        "PLAYING should be resolved, got: {}",
        playing
    );

    // Test 2: Contractions
    let (ive, _) = g2p.g2p("I've").unwrap();
    println!("I've: {}", ive);
    assert!(!ive.contains("❓"), "I've should be resolved, got: {}", ive);

    // Test 3: Dashes
    // em-dash — (U+2014) and hyphen -
    let (dash, _) = g2p.g2p("word - word — word").unwrap();
    println!("Dash: {}", dash);
    // misaki 0.9.4 gives exactly this, verified against the oracle:
    //
    //     "word - word — word" -> wˈɜɹd ❓ wˈɜɹd — wˈɜɹd
    //
    // The two dashes are NOT alike. spaCy tags the em-dash `:`, which puts
    // it in PUNCT_TAGS and renders it —; it tags the plain hyphen HYPH,
    // which is not punctuation, so the lexicon misses it and it comes back
    // unresolved and prints as `unk`. This assertion used to forbid `unk`
    // altogether, which was the pre-fork behaviour.
    assert!(dash.contains('—'), "em-dash dropped: {dash}");
    assert_eq!(
        dash, "wˈɜɹd ❓ wˈɜɹd — wˈɜɹd",
        "does not match misaki 0.9.4"
    );
}

#[test]
fn test_kokoros_basic() {
    let g2p = G2P::new(Language::EnglishUS);
    let cases = vec![
        "hello",
        "world",
        "the quick brown fox",
        "testing phonemization",
        "Hello, world!",
        "123",
        "restriction",
        "restrictions",
        "",
    ];
    for text in cases {
        let (p, _) = g2p.g2p(text).unwrap();
        println!("'{}' -> '{}'", text, p);
        if !text.is_empty() {
            assert!(!p.is_empty(), "Failed for '{}'", text);
        }
    }
}

#[test]
fn test_kokoros_numbers() {
    let g2p = G2P::new(Language::EnglishUS);
    let cases = vec![
        "CHAPTER XIV",
        "CHAPTER 14",
        "CHAPTER 123",
        "I have 5 apples and 42 oranges",
        "The year 2024",
        "1234567890",
        "CHAPTER I",
        "CHAPTER II",
        "CHAPTER III",
        "CHAPTER IV",
        "CHAPTER V",
        "CHAPTER X",
        "CHAPTER XX",
        "CHAPTER XXX",
        "In 2024, CHAPTER XIV had 42 pages.",
        "The price is $123.45",
        "Temperature: -5°C",
        "Score: 100%",
        "Version 2.0",
        "3.14159",
    ];
    for text in cases {
        let (p, _) = g2p.g2p(text).unwrap();
        println!("'{}' -> '{}'", text, p);
        assert!(!p.is_empty(), "Failed for '{}'", text);
    }
}

#[test]
fn test_kokoros_utf8_and_special() {
    let g2p = G2P::new(Language::EnglishUS);
    let cases = vec![
        "café",
        "naïve",
        "résumé",
        "Zürich",
        "São Paulo",
        "Müller",
        "北京",
        "こんにちは",
        "Здравствуй",
        "مرحبا",
        "🎉🎊🎈",
        // Control chars
        "\x00\x01\x02",
        // Mixed scripts
        "Hello 世界",
        "123中文",
        "English123中文",
        // Zero-width characters
        "hello\u{200B}world", // zero-width space
        "hello\u{200C}world", // zero-width non-joiner
        "hello\u{200D}world", // zero-width joiner
        // Combining characters
        "caf\u{00E9}",  // é as combining character
        "na\u{00EF}ve", // ï as combining character
    ];
    for text in cases {
        let (p, _) = g2p.g2p(text).unwrap();
        println!("'{}' -> '{}'", text, p);
        // Some might be empty/unknown depending on handling, but shouldn't crash
    }
}

#[test]
fn test_kokoros_punctuation() {
    let g2p = G2P::new(Language::EnglishUS);
    let cases = vec![
        "Hello—world", // em dash
        "Hello–world", // en dash
        "Hello…world", // ellipsis
        "\"quoted text\"",
        "'single quotes'",
        "«French quotes»",
        "„German quotes„",
        "「Japanese quotes」",
        "Dr. Smith",
        "Mr. Jones",
        "Mrs. Brown",
        "Ms. Davis",
        "etc.",
        "U.S.A.",
        "Ph.D.",
        "A.I.",
        "NASA",
        "FBI",
        "   ",
        "\n\n",
        "\t\t",
        "\r\n",
    ];
    for text in cases {
        let (p, _) = g2p.g2p(text).unwrap();
        println!("'{}' -> '{}'", text, p);
    }
}

#[test]
fn test_kokoros_punctuation_preserved() {
    // Kokoro's vocab encodes ";:,.!?" as distinct token ids (1-6).
    // G2P must emit each character literally in the phoneme stream so
    // downstream tokenizers can map them; collapsing to space loses the
    // sentence-end / clause-breath / question-intonation cues the
    // synthesis model relies on. Other ASCII punctuation (parens,
    // dashes, ellipsis, …) keeps the prior single-space fallback.
    let g2p = G2P::new(Language::EnglishUS);

    let (phonemes, _) = g2p.g2p("Hello, world.").unwrap();
    assert!(phonemes.contains(','), "comma dropped: {phonemes:?}");
    assert!(phonemes.contains('.'), "period dropped: {phonemes:?}");

    for c in [';', ':', '!', '?'] {
        let input = format!("test {c}");
        let (out, _) = g2p.g2p(&input).unwrap();
        assert!(out.contains(c), "{c:?} dropped: {out:?}");
    }

    // Parentheses ARE preserved. This assertion used to require the
    // opposite, which was the pre-fork behaviour: misaki maps spaCy's
    // -LRB-/-RRB- tags to '(' and ')' via PUNCT_TAG_PHONEMES
    // (`en.py:598-599`), and misaki 0.9.4 gives
    //
    //     "hello (world)" -> həlˈO (wˈɜɹld)
    //
    // verified against the oracle. The gate is misaki's output, so the test
    // follows it rather than the other way round.
    let (out, _) = g2p.g2p("hello (world)").unwrap();
    assert!(out.contains('('), "( dropped: {out:?}");
    assert!(out.contains(')'), ") dropped: {out:?}");
}

#[test]
fn test_kokoros_long_text() {
    let g2p = G2P::new(Language::EnglishUS);
    // Reduced to 100 to check if it crashes
    let long_text = "a".repeat(1000);
    let (p, _) = g2p.g2p(&long_text).unwrap();
    assert!(!p.is_empty());
}

/// Records each word misaki gives the fallback.
struct Recorder(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

impl Fallback for Recorder {
    fn phonemize(&self, word: &str) -> Result<String, FallbackError> {
        self.0.lock().unwrap().push(word.to_string());
        Ok("zz".to_string())
    }
}

fn sent(text: &str) -> Vec<String> {
    let got = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let g2p = G2P::with_fallback(Language::EnglishUS, Some(Box::new(Recorder(got.clone()))));
    g2p.g2p(text).unwrap();
    let v = got.lock().unwrap().clone();
    v
}

#[test]
fn test_fallback_gets_no_contraction_piece_nor_words_joined_by_a_dash() {
    let text = "The moral is—‘Be good.’ Fetch it, lad!—Here. You can't—I won’t; she couldn’t.";
    assert_eq!(sent(text), Vec::<String>::new());
    assert_eq!(sent("Fetch it, Talena!—Here."), ["Talena"]);
    assert_eq!(sent("Gor’s moons"), ["Gor’s"]);
}
