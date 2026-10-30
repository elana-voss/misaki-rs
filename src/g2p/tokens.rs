//! How tokens group into words, their lookup keys, punctuation and context (misaki's `retokenize`).
use super::*;

impl G2P {
    /// misaki's punctuation test (`retokenize`):
    ///
    /// ```text
    ///     elif tk.tag in PUNCT_TAGS and not all(97 <= ord(c.lower()) <= 122 for c in tk.text):
    /// ```
    ///
    /// with PUNCT_TAGS = {".", ",", "-LRB-", "-RRB-", "``", "\"\"", "''", ":",
    /// "$", "#", "NFP"}. Such a token gets its phonemes in `retokenize` and is
    /// therefore appended STANDALONE, never merged into a neighbouring word.
    ///
    /// This is the rule, and no character test substitutes for it: spaCy tags
    /// the `_` in `_like` as UH and the one in `_it` as NFP, so misaki merges
    /// the first and not the second from structurally identical input. Likewise
    /// `dogs'` has tag POS and merges, while `Thomas’` has `''` and does not.
    pub(super) fn is_punct_token(tk: &MToken) -> bool {
        const PUNCT_TAGS: [&str; 11] = [
            ".", ",", "-LRB-", "-RRB-", "``", "\"\"", "''", ":", "$", "#", "NFP",
        ];
        if !PUNCT_TAGS.contains(&tk.tag.as_str()) {
            return false;
        }
        // ... and not entirely a-z, so a word that happens to carry a
        // punctuation tag is still a word.
        !tk.text.chars().all(|c| {
            let l = c.to_lowercase().next().unwrap_or(c);
            ('a'..='z').contains(&l)
        })
    }

    /// Port of misaki's module-level `merge_tokens` (`en.py`).
    ///
    /// Collapses a run of adjacent subtokens into the single word misaki would
    /// have produced. The text is rebuilt with the interior whitespace put
    /// back, the group inherits the LAST token's trailing whitespace, and the
    /// tag comes from the "heaviest" member -- misaki scores each token's text
    /// as `sum(1 if c == c.lower() else 2 for c in text)` and takes the
    /// maximum, so `five-storied` takes `storied`'s JJ (7 beats 4 and 1) and
    /// `It’s` takes `It`'s PRP (3 beats 2).
    pub(super) fn merge_text_tag(group: &[MToken]) -> (String, String, String) {
        let mut text = String::new();
        for tk in &group[..group.len() - 1] {
            text.push_str(&tk.text);
            text.push_str(&tk.whitespace);
        }
        text.push_str(&group[group.len() - 1].text);

        let weight = |t: &str| -> usize {
            t.chars()
                .map(|c| if c.to_lowercase().next() == Some(c) { 1 } else { 2 })
                .sum()
        };
        let tag = group
            .iter()
            .max_by_key(|tk| weight(&tk.text))
            .map(|tk| tk.tag.clone())
            .unwrap_or_default();

        let ws = group[group.len() - 1].whitespace.clone();
        (text, tag, ws)
    }

    /// Port of the grouping half of misaki's `G2P.retokenize` (`en.py`).
    ///
    /// misaki groups subtokens into words by ADJACENCY, not by character
    /// class: a token with no trailing whitespace opens a group, and each
    /// following token joins it while the group's last member still has no
    /// trailing whitespace.
    ///
    /// ```text
    ///     elif words and isinstance(words[-1], list) and not words[-1][-1].whitespace:
    ///         tk._.is_head = False; words[-1].append(tk)
    ///     else:
    ///         words.append(tk if tk.whitespace else [tk])
    /// ```
    ///
    /// This is what lets a finer tokenizer than spaCy's stay correct: `Mr` `.`
    /// and `It` `’` `s` and `five` `-` `storied` are each adjacent runs, so they
    /// merge back into `Mr.`, `It’s` and `five-storied`. Returns index ranges
    /// into `tokens`.
    pub(super) fn group_by_adjacency(tokens: &[MToken]) -> Vec<std::ops::Range<usize>> {
        // misaki's FIRST branch takes precedence over adjacency:
        //
        //     if tk._.alias is not None or tk.phonemes is not None:
        //         words.append(tk)
        //
        // and `retokenize` has already given punctuation its phonemes by then,
        // so a punctuation mark is always its own word. `is_punct_token` is
        // misaki's own test, on the TAG; the character approximation that used
        // to be here could not tell `_like` from `_it`.
        let is_standalone = |tk: &MToken, _is_last: bool| -> bool {
            // misaki's first branch is `if tk._.alias is not None or
            // tk.phonemes is not None`, so an aliased token stands alone too.
            tk.underscore().alias.is_some() || Self::is_punct_token(tk)
        };

        let mut groups: Vec<std::ops::Range<usize>> = Vec::new();
        let mut open = false; // is the last group still extensible?
        for i in 0..tokens.len() {
            if is_standalone(&tokens[i], i + 1 == tokens.len()) {
                // ... unless joining it to the open group spells an
                // abbreviation spaCy would never have split. Our tokenizer
                // splits on the period unconditionally, so `Mr` + `.` must be
                // put back together; spaCy's own exception list decides.
                let rejoin = tokens[i].text == "." && open;
                if rejoin {
                    let g = groups.last().unwrap().clone();
                    let mut candidate = String::new();
                    for tk in &tokens[g.clone()] {
                        candidate.push_str(&tk.text);
                        candidate.push_str(&tk.whitespace);
                    }
                    candidate.push('.');
                    if crate::abbrev::is_period_abbreviation(candidate.trim()) {
                        let g = groups.last_mut().unwrap();
                        *g = g.start..i + 1;
                        open = tokens[i].whitespace.is_empty();
                        continue;
                    }
                }
                groups.push(i..i + 1);
                open = false;
                continue;
            }
            if open && tokens[groups.last().unwrap().end - 1].whitespace.is_empty() {
                let g = groups.last_mut().unwrap();
                *g = g.start..i + 1;
            } else {
                groups.push(i..i + 1);
                // A token that already carries trailing whitespace is a
                // finished word: misaki appends it bare rather than as a list,
                // and only a list can be extended.
                open = tokens[i].whitespace.is_empty();
            }
        }
        groups
    }

    /// misaki's lookup-key normalisation (`Lexicon.__call__`): curly single
    /// quotes become straight ones, then NFKC. The lexicons are keyed with
    /// straight apostrophes.
    ///
    /// NFKC itself is not applied here -- it would need a unicode-normalisation
    /// dependency, and the characters it folds that matter for English
    /// (ligatures, full-width forms) do not appear in the corpora. The quote
    /// substitution is the part that changes lookups.
    pub(super) fn normalize_lookup(text: &str) -> String {
        text.replace('\u{2018}', "'").replace('\u{2019}', "'")
    }

    /// misaki's punctuation phonemes (`en.py:598-599`): the PUNCT_TAG_PHONEMES
    /// mapping, then keep only characters in PUNCTS.
    ///
    /// ```text
    ///     tk.phonemes = PUNCT_TAG_PHONEMES.get(
    ///         tk.tag, ''.join(c for c in tk.text if c in PUNCTS))
    /// ```
    ///
    /// with PUNCTS = ';:,.!?\u2014\u2026"\u201c\u201d' and `` -> \u201c, '' -> \u201d, -LRB- -> (,
    /// -RRB- -> ). An empty result is correct for a symbol in no class (`*`).
    pub(super) fn punct_phonemes(text: &str, tag: &str) -> String {
        const PUNCTS: &str = ";:,.!?\u{2014}\u{2026}\"\u{201c}\u{201d}";
        // PUNCT_TAG_PHONEMES first, by TAG. This is keyed on the tag and not on
        // the character, which is what distinguishes an opening quote from a
        // closing one: in `'Single quotes'` spaCy tags the first apostrophe ``
        // and the second '', so misaki renders “ then ” from identical
        // characters. A straight `"` is likewise “ or ” depending on side.
        match tag {
            "-LRB-" => return "(".to_string(),
            "-RRB-" => return ")".to_string(),
            "``" => return "\u{201c}".to_string(),
            "''" | "\"\"" => return "\u{201d}".to_string(),
            _ => {}
        }
        // Otherwise keep only the characters that are in PUNCTS.
        text.chars().filter(|c| PUNCTS.contains(*c)).collect()
    }

    /// Port of misaki's `G2P.token_context` (`en.py`).
    ///
    /// Decides what the token to the LEFT should assume about what follows it:
    /// whether the next sound is a vowel (`the` -> `\u00f0i` vs `\u00f0\u0259`, `a` -> `\u0250`), and
    /// whether the next word is `to` (`to` -> `t\u028a` vs `t\u0259`). Both are decided
    /// from the RESOLVED PHONEMES, never from spelling.
    pub(super) fn token_context(
        ctx: &crate::lexicon::TokenContext,
        ps: Option<&str>,
        tk: &MToken,
    ) -> crate::lexicon::TokenContext {
        const VOWELS: &str = "AIOQWYaiu\u{e6}\u{251}\u{252}\u{254}\u{259}\u{25b}\u{25c}\u{26a}\u{28a}\u{28c}\u{1d7b}";
        const CONSONANTS: &str = "bdfhjklmnpstvwz\u{f0}\u{14b}\u{261}\u{279}\u{27e}\u{283}\u{292}\u{2a4}\u{2a7}\u{3b8}";
        const NON_QUOTE_PUNCTS: &str = "!,.:;?\u{2014}\u{2026}";

        // `next(..., vowel)` in Python: if no character belongs to any of the
        // three classes, the incoming value survives unchanged.
        let mut future_vowel = ctx.future_vowel;
        if let Some(ps) = ps {
            for c in ps.chars() {
                if NON_QUOTE_PUNCTS.contains(c) {
                    future_vowel = None;
                    break;
                } else if VOWELS.contains(c) {
                    future_vowel = Some(true);
                    break;
                } else if CONSONANTS.contains(c) {
                    future_vowel = Some(false);
                    break;
                }
            }
        }

        let future_to = tk.text == "to"
            || tk.text == "To"
            || (tk.text == "TO" && (tk.tag == "TO" || tk.tag == "IN"));

        crate::lexicon::TokenContext { future_vowel, future_to }
    }

    pub fn tokenize(&self, text: &str) -> Vec<MToken> {
        self.tokenize_with_offsets(text)
            .into_iter()
            .map(|(tk, _)| tk)
            .collect()
    }

    /// Tokenize, returning each subtoken alongside its byte range in `text`.
    ///
    /// Needed to map [`LinkMarker`] byte ranges (from
    /// [`G2P::preprocess_links`]) onto specific [`MToken`]s after
    /// subtokenization. The Python equivalent uses spaCy's
    /// `Alignment.from_strings` against a whitespace-split token list; we use
    /// byte offsets instead, which is more robust to subtoken splits.
    pub fn tokenize_with_offsets(
        &self,
        text: &str,
    ) -> Vec<(MToken, std::ops::Range<usize>)> {
        let word_boundary_regex = Regex::new(r"\S+").unwrap();
        let mut tokens = Vec::new();

        for mat in word_boundary_regex.find_iter(text) {
            let word = mat.as_str();
            let word_start = mat.start();
            let subtokens: Vec<fancy_regex::Match> =
                self.subtoken_regex.find_iter(word).flatten().collect();

            if subtokens.is_empty() {
                let tk = MToken::new(word.to_string(), "NN".to_string(), String::new());
                tokens.push((tk, word_start..mat.end()));
            } else {
                for sub in subtokens {
                    let s = word_start + sub.start();
                    let e = word_start + sub.end();
                    let tk =
                        MToken::new(sub.as_str().to_string(), "NN".to_string(), String::new());
                    tokens.push((tk, s..e));
                }
            }
        }

        // Fill in each token's trailing whitespace from the SOURCE TEXT.
        //
        // misaki carries spaCy's `token.whitespace_`, i.e. the actual
        // characters between this token and the next -- empty when the next
        // token is adjacent. The assembled phoneme string is
        // `''.join(phonemes + whitespace)` (`en.py:711`), so hardcoding " "
        // here produced "b\u02c8\u0251\u0279kt . " where misaki gives
        // "b\u02c8\u0251\u0279kt.", and no phoneme string could ever match.
        // Byte ranges from the regex are always char boundaries, so slicing is
        // safe.
        for i in 0..tokens.len() {
            let gap_start = tokens[i].1.end;
            let gap_end = if i + 1 < tokens.len() {
                tokens[i + 1].1.start
            } else {
                text.len()
            };
            if gap_start < gap_end {
                tokens[i].0.whitespace = text[gap_start..gap_end].to_string();
            }
        }

        tokens
    }
}
