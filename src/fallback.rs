use thiserror::Error;

#[cfg(feature = "espeak")]
use std::sync::Mutex;
#[cfg(feature = "espeak")]
use espeak_rs::text_to_phonemes;

#[cfg(feature = "espeak")]
static ESPEAK_MUTEX: Mutex<()> = Mutex::new(());

#[derive(Error, Debug)]
pub enum FallbackError {
    #[error("espeak error for '{word}': {error}")]
    Espeak {
        word: String,
        error: String,
    },
    #[error("no phonemes matched for '{word}'")]
    NoPhonemes {
        word: String,
    },
    #[error("mutex poisoned: {0}")]
    MutexPoisoned(String),
}

/// Trait for OOV (out-of-vocabulary) word fallback mechanisms
pub trait Fallback: Send + Sync {
    /// Convert unknown word to phonemes
    /// Returns phonemes
    /// Note: espeak-ng is rule-based and always produces output
    fn phonemize(&self, word: &str) -> Result<String, FallbackError>;
}

/// espeak-ng based fallback (requires `espeak` feature)
#[cfg(feature = "espeak")]
pub struct EspeakFallback {
    british: bool,
}

#[cfg(feature = "espeak")]
impl EspeakFallback {
    pub fn new(british: bool) -> Result<Self, String> {
        Ok(Self { british })
    }

    /// misaki 0.9.4's `EspeakFallback.__call__` after phonemizer: eSpeak's IPA, with `^` tying
    /// the letters of one phoneme, turned into Kokoro's symbols.
    fn convert_espeak_to_misaki(&self, espeak_ipa: &str) -> String {
        // E2M, longest first as misaki sorts it
        const E2M: [(&str, &str); 20] = [
            ("ʔˌn\u{329}", "ʔn"), ("ʔn\u{329}", "ʔn"), ("a^ɪ", "I"), ("a^ʊ", "W"), ("d^ʒ", "ʤ"),
            ("e^ɪ", "A"), ("t^ʃ", "ʧ"), ("ɔ^ɪ", "Y"), ("ə^l", "ᵊl"), ("ʲo", "jo"), ("ʲə", "jə"),
            ("e", "A"), ("ʲ", ""), ("ɚ", "əɹ"), ("r", "ɹ"), ("x", "k"), ("ç", "k"), ("ɐ", "ə"),
            ("ɬ", "l"), ("\u{303}", ""),
        ];
        let mut ps = espeak_ipa.trim().to_string();
        for (old, new) in E2M {
            ps = ps.replace(old, new);
        }
        // a syllabic consonant gets a schwa before it
        let mut out = String::new();
        for c in ps.chars() {
            if c == '\u{329}' {
                if let Some(prev) = out.pop() {
                    if prev.is_whitespace() {
                        out.push(prev);
                    } else {
                        out.push('ᵊ');
                        out.push(prev);
                    }
                }
            } else {
                out.push(c);
            }
        }
        let mut ps = out;
        if self.british {
            ps = ps.replace("e^ə", "ɛː").replace("iə", "ɪə").replace("ə^ʊ", "Q");
        } else {
            ps = ps.replace("o^ʊ", "O").replace("ɜːɹ", "ɜɹ").replace("ɜː", "ɜɹ").replace("ɪə", "iə").replace('ː', "");
        }
        // Kokoro v1.0's flap and glottal stop (misaki: version != '2.0')
        ps.replace('o', "ɔ").replace('ɾ', "T").replace('ʔ', "t").replace('^', "")
    }
}

/// eSpeak's IPA for `text` with `^` inside multi-letter phonemes, as phonemizer asks for it with
/// `tie='^'`; espeak-rs's own call has no way to ask for a tie, so espeak-ng is called directly.
#[cfg(feature = "espeak")]
fn espeak_ipa(text: &str, voice: &str) -> Result<String, String> {
    use std::ffi::{CStr, CString, c_void};
    // espeak-rs finds espeak-ng-data and initialises espeak-ng on its first call of a non-empty text
    static INIT: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    INIT.get_or_init(|| text_to_phonemes("a", voice, None, true, false).map(|_| ()).map_err(|e| format!("{e:?}")))
        .clone()?;
    let voice = CString::new(voice).map_err(|e| e.to_string())?;
    // a NUL would end espeak-ng's text there, and CString refuses one
    let text = CString::new(text.replace('\0', "")).map_err(|e| e.to_string())?;
    let mode = (('^' as i32) << 8) | espeak_rs_sys::espeakPHONEMES_TIE as i32 | espeak_rs_sys::espeakPHONEMES_IPA as i32;
    let mut clauses = Vec::new();
    // SAFETY: espeak-ng is used under ESPEAK_MUTEX; it advances `ptr` through `text`, which outlives
    // the loop, and returns a string it owns until its next call, copied at once.
    unsafe {
        if espeak_rs_sys::espeak_SetVoiceByName(voice.as_ptr()) != espeak_rs_sys::espeak_ERROR_EE_OK {
            return Err(format!("no eSpeak voice {voice:?}"));
        }
        let mut ptr = text.as_ptr() as *const c_void;
        while !ptr.is_null() {
            let res = espeak_rs_sys::espeak_TextToPhonemes(&mut ptr, espeak_rs_sys::espeakCHARS_UTF8 as i32, mode);
            if res.is_null() {
                break;
            }
            clauses.push(CStr::from_ptr(res).to_string_lossy().trim().to_string());
        }
    }
    Ok(clauses.into_iter().filter(|c| !c.is_empty()).collect::<Vec<_>>().join(" "))
}

#[cfg(feature = "espeak")]
impl Fallback for EspeakFallback {
    fn phonemize(&self, word: &str) -> Result<String, FallbackError> {
        let _lock = ESPEAK_MUTEX
            .lock()
            .map_err(|e| FallbackError::MutexPoisoned(format!("{:?}", e)))?;
        let voice = if self.british { "en" } else { "en-us" };
        let ipa = espeak_ipa(word, voice).map_err(|error| FallbackError::Espeak { word: word.to_string(), error })?;
        if ipa.is_empty() {
            return Err(FallbackError::NoPhonemes { word: word.to_string() });
        }
        Ok(self.convert_espeak_to_misaki(&ipa))
    }
}

#[cfg(all(test, feature = "espeak"))]
mod tests {
    use super::*;

    #[test]
    fn espeak_symbols_become_kokoros() {
        let us = EspeakFallback::new(false).unwrap();
        // misaki 0.9.4 with phonemizer gives these for Gors, Pakur and Kor-ob-a
        assert_eq!(us.convert_espeak_to_misaki("ɡˈoː^ɹz"), "ɡˈɔɹz");
        assert_eq!(us.convert_espeak_to_misaki("pˈækɜː"), "pˈækɜɹ");
        assert_eq!(us.convert_espeak_to_misaki("kˈoː^ɹɑːbˈe^ɪ"), "kˈɔɹɑbˈA");
        assert_eq!(us.convert_espeak_to_misaki("bˈʌɾɚ"), "bˈʌTəɹ");
    }

    #[test]
    fn espeak_output_has_ties_and_no_full_stop() {
        let us = EspeakFallback::new(false).unwrap();
        assert_eq!(us.phonemize("Gors").unwrap(), "ɡˈɔɹz");
    }

    #[test]
    fn an_unknown_word_with_a_known_ending_goes_to_espeak_whole() {
        let g2p = crate::G2P::new(crate::Language::EnglishUS);
        assert_eq!(g2p.g2p("Gor's").unwrap().0.trim(), "ɡˈɔɹz");
    }

    #[test]
    fn test_espeak_fallback() {
        let fallback = EspeakFallback::new(false).expect("espeak should initialize");

        // Test unknown word - espeak ALWAYS returns something
        let phonemes = fallback.phonemize("ilili").unwrap();
        assert!(!phonemes.is_empty());

        // Verify it doesn't spell out character-by-character
        assert!(!phonemes.contains("ˈɛl"));  // Should not have spelled-out 'L'
    }

    #[test]
    fn test_espeak_nonsense_word() {
        let fallback = EspeakFallback::new(false).unwrap();

        // espeak handles even nonsense words
        let phonemes = fallback.phonemize("xyzqwop").unwrap();
        assert!(!phonemes.is_empty(), "espeak should phonemize nonsense words");
    }

    #[test]
    fn test_espeak_phonemes_beat() {
        let fallback = EspeakFallback::new(false).unwrap();
        let phonemes = fallback.phonemize("beat").unwrap();
        // Misaki for beat should probably be bˈit or similar
        assert!(
            phonemes.contains("ˈi"),
            "Should contain stressed i, got: {}",
            phonemes
        );
    }

    #[test]
    fn test_espeak_american_vs_british() {
        let us = EspeakFallback::new(false).unwrap();
        let gb = EspeakFallback::new(true).unwrap();

        // Test word with different pronunciations
        let us_phonemes = us.phonemize("schedule").unwrap();
        let gb_phonemes = gb.phonemize("schedule").unwrap();

        assert!(!us_phonemes.is_empty());
        assert!(!gb_phonemes.is_empty());
        // Pronunciations should differ (US: sked-, GB: shed-)
        // In IPA: US often starts with 'sk', GB often starts with 'ʃ'
        assert!(us_phonemes.contains("sk"), "US schedule usually has 'sk', got: {}", us_phonemes);
        assert!(gb_phonemes.contains("ʃ"), "GB schedule usually starts with 'ʃ', got: {}", gb_phonemes);
    }
}
