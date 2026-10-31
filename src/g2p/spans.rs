//! The pieces of a word of several (misaki's multi-token loop in `G2P.__call__`).
use super::*;

impl G2P {
    /// The phonemes of each piece of the group `g` of `tokens` (None: unresolved), and the whole
    /// word's from the fallback when misaki gives it the whole word (a piece no dictionary
    /// answers); `text` is the group's text.
    pub(super) fn spans(
        &self,
        tokens: &[MToken],
        g: std::ops::Range<usize>,
        contexts: &[crate::lexicon::TokenContext],
        text: &str,
    ) -> Result<(Vec<Option<String>>, Option<String>), G2PError> {
        let group = &tokens[g.clone()];
        let n = g.len();
        let mut res: Vec<Option<String>> = vec![None; n];
        let mut left = 0usize;
        let mut right = n;
        let mut should_fallback = false;
        // a piece of marks only, not junk: the group is words joined by marks
        let marked = group.iter().any(|tk| {
            !tk.text.is_empty()
                && !tk.text.chars().any(char::is_alphanumeric)
                && !tk.text.chars().all(|c| SUBTOKEN_JUNKS.contains(c))
        });
        while left < right {
            // misaki only merges a span none of whose members has
            // resolved yet.
            let free = res[left..right].iter().all(|r| r.is_none());
            let hit = if free {
                let sub = &tokens[g.start + left..g.start + right];
                let (t, tg, _) = Self::merge_text_tag(sub);
                let k = Self::normalize_lookup(&t);
                let st = if k == k.to_lowercase() {
                    None
                } else if k == k.to_uppercase() {
                    Some(self.lexicon.cap_stresses.1)
                } else {
                    Some(self.lexicon.cap_stresses.0)
                };
                // the context at the SPAN's own left edge, not the
                // group's: misaki carries a rolling ctx through its
                // loop, so the inner `the` of `out-of-the-way` sees
                // what follows IT (`way`, a consonant -> ðə) and not
                // what follows the whole word.
                self.lexicon
                    .get_word(&k, &tg, st, Some(&contexts[g.start + left]))
                    .map(|(ps, _)| ps)
            } else {
                None
            };
            if let Some(ps) = hit {
                res[left] = Some(ps);
                for k in left + 1..right {
                    res[k] = Some(String::new());
                }
                right = left;
                left = 0;
            } else if left + 1 < right {
                left += 1;
            } else {
                right -= 1;
                if res[right].is_none() {
                    let t = &tokens[g.start + right].text;
                    if !t.is_empty() && t.chars().all(|c| SUBTOKEN_JUNKS.contains(c)) {
                        res[right] = Some(String::new());
                    } else if let (Some(fallback), true) = (&self.fallback, marked) {
                        // words joined by a mark ("lad!—Here", "is—‘Be") are each
                        // their own: the mark said as without a fallback, a word
                        // no dictionary answers given to the fallback alone
                        res[right] = if !t.chars().any(char::is_alphanumeric) {
                            Some(group[right].phonemes.clone().unwrap_or_default())
                        } else {
                            match fallback.phonemize(t) {
                                Err(FallbackError::NoPhonemes { .. }) => Some(String::new()),
                                other => Some(other?),
                            }
                        };
                    } else if self.fallback.is_some() {
                        should_fallback = true;
                        break;
                    }
                }
                left = 0;
            }
        }
        // With a fallback, misaki gives the WHOLE word to it as soon
        // as one piece stays unknown (`should_fallback`), so `Gor's`
        // is eSpeak's ɡˈɔɹz and not eSpeak's `Gor` stitched to the
        // lexicon's `'s`. Without one, anything still unresolved
        // stays unresolved: merge_tokens substitutes `unk` for None,
        // and the stitched path below assembles whatever `res` holds.
        let whole: Option<String> = match (&self.fallback, should_fallback) {
            (Some(fallback), true) => match fallback.phonemize(text) {
                Err(FallbackError::NoPhonemes { .. }) => None,
                other => Some(other?),
            },
            _ => None,
        };
        Ok((res, whole))
    }
}
