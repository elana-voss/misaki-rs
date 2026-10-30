use crate::fallback::{Fallback, FallbackError};
use crate::language::Language;
#[cfg(feature = "espeak")]
use crate::fallback::EspeakFallback;
use crate::languages::{LanguageRules, english::English};
use crate::lexicon::Lexicon;
use thiserror::Error;
use crate::tagger::PerceptronTagger;
use crate::token::MToken;
use fancy_regex::Regex as FancyRegex;
use num2words::Num2Words;
use regex::Regex;
use std::collections::HashMap;

mod links;
mod numbers;
mod tokens;
pub use links::{LinkFeature, LinkMarker};

#[derive(Error, Debug)]
pub enum G2PError {
    #[error("fallback error: {0}")]
    Fallback(#[from] FallbackError),
}


pub struct G2P {
    pub lexicon: Lexicon,
    pub unk: String,
    subtoken_regex: FancyRegex,
    link_regex: Regex,
    /// The perceptron tagger this crate shipped with. No longer used for
    /// pronunciation -- `spacy_tagger` is, because misaki's output depends on
    /// spaCy's exact tags -- but kept so the two remain comparable.
    #[allow(dead_code)]
    tagger: PerceptronTagger,
    spacy_tagger: crate::spacy_tagger::Tagger,
    spacy_tokenizer: crate::spacy_tokenizer::SpacyTokenizer,
    rules: Box<dyn LanguageRules>,
    fallback: Option<Box<dyn Fallback>>,
}

impl G2P {
    pub fn new(lang: Language) -> Self {
        // misaki's SUBTOKEN_REGEX (`en.py:55`), complete and in order, since
        // regex alternation takes the first match:
        //
        //   ^['‘’]+
        //   | \p{Lu}(?=\p{Lu}\p{Ll})
        //   | (?:^-)?(?:\d?[,.]?\d)+
        //   | [-_]+
        //   | ['‘’]{2,}
        //   | \p{L}*?(?:['‘’]\p{L})*?\p{Ll}(?=\p{Lu})
        //   | \p{L}+(?:['‘’]\p{L})*
        //   | [^-_\p{L}'‘’\d]
        //   | ['‘’]+$
        //
        // The two alternatives carrying lookahead are what split camel case,
        // and they were missing: `iPhone` stayed whole, missed the lexicon and
        // resolved to nothing, where misaki splits it `i` + `Phone` and gets
        // ˈIfˌOn. fancy_regex supplies the lookahead the regex crate lacks.
        //
        // Two bugs in the character classes are fixed here too. The
        // word-internal apostrophe class held three copies of the STRAIGHT
        // apostrophe where misaki has straight plus both curly, and the
        // catch-all excluded only the straight one -- so a curly right
        // apostrophe became its own token and every curly possessive and
        // contraction in real prose came apart.
        let subtoken_regex = FancyRegex::new(concat!(
            r"^['‘’]+",
            r"|\p{Lu}(?=\p{Lu}\p{Ll})",
            r"|(?:^-)?(?:\d?[,.]?\d)+",
            r"|[-_]+",
            r"|['‘’]{2,}",
            r"|\p{L}*?(?:['‘’]\p{L})*?\p{Ll}(?=\p{Lu})",
            r"|\p{L}+(?:['‘’]\p{L})*",
            r"|[^-_\p{L}'‘’\d]",
            r"|['‘’]+$",
        ))
        .unwrap();

        let weights_json = include_str!("resources/tagger/weights.json");
        let classes_txt = include_str!("resources/tagger/classes.txt");
        let tags_json = include_str!("resources/tagger/tags.json");

        let rules: Box<dyn LanguageRules> = match lang {
            Language::EnglishUS | Language::EnglishGB => Box::new(English),
            // Language::Italian => Box::new(Italian),
        };

        #[cfg(feature = "espeak")]
        let fallback: Option<Box<dyn Fallback>> = match EspeakFallback::new(lang == Language::EnglishGB) {
            Ok(fb) => Some(Box::new(fb)),
            Err(e) => {
                log::warn!("espeak-ng fallback unavailable: {}", e);
                None
            }
        };
        #[cfg(not(feature = "espeak"))]
        let fallback: Option<Box<dyn Fallback>> = None;

        // Mirrors `LINK_REGEX = re.compile(r'\[([^\]]+)\]\(([^\)]*)\)')`
        // from misaki/en.py.
        let link_regex = Regex::new(r"\[([^\]]+)\]\(([^\)]*)\)").unwrap();

        Self {
            lexicon: Lexicon::new(lang),
            unk: "❓".to_string(),
            subtoken_regex,
            link_regex,
            tagger: PerceptronTagger::new(weights_json, classes_txt, tags_json),
            // The ported spaCy tagger. Kept alongside the perceptron rather
            // than replacing it so the old one stays available for comparison.
            spacy_tagger: crate::spacy_tagger::Tagger::load(),
            spacy_tokenizer: crate::spacy_tokenizer::SpacyTokenizer::new(),
            rules,
            fallback,
        }
    }

    pub fn with_fallback(lang: Language, fallback: Option<Box<dyn Fallback>>) -> Self {
        let mut g2p = Self::new(lang);
        g2p.fallback = fallback;
        g2p
    }

    /// Legacy preprocess shape, kept for API compatibility.
    ///
    /// The third return value (token-index → feature) is intentionally empty
    /// — markers are now exposed via [`G2P::preprocess_links`] using byte
    /// offsets, which compose with our subtokenizer (Python misaki keys
    /// features by spaCy-tokenized index; we don't have spaCy).
    pub fn preprocess(&self, text: &str) -> (String, Vec<String>, HashMap<usize, String>) {
        let (stripped, _) = self.preprocess_links(text);
        let tokens: Vec<String> = stripped.split_whitespace().map(|s| s.to_string()).collect();
        (stripped, tokens, HashMap::new())
    }



    pub fn g2p(&self, text: &str) -> Result<(String, Vec<MToken>), G2PError> {
        // Parse `[text](feature)` markers and attach them to the subtokens
        // whose byte range falls inside the marker span. Mirrors the
        // `Lexicon.tokenize` feature-application pass in misaki/en.py.
        let (processed_text, markers) = self.preprocess_links(text);
        let mut tokens_with_spans = self.tokenize_with_offsets(&processed_text);

        for marker in &markers {
            let mut first_in_span = true;
            for (tk, span) in tokens_with_spans.iter_mut() {
                if span.start >= marker.byte_start && span.end <= marker.byte_end {
                    match &marker.feature {
                        LinkFeature::Stress(s) => {
                            tk.underscore_mut().stress = Some(*s);
                        }
                        LinkFeature::Phonemes(p) => {
                            // Only the head subtoken receives the override;
                            // trailing subtokens are blanked so the main loop
                            // skips them. Matches Python's
                            // `phonemes = v.lstrip('/') if i == 0 else ''`.
                            if first_in_span {
                                tk.phonemes = Some(p.clone());
                            } else {
                                tk.phonemes = Some(String::new());
                            }
                        }
                        LinkFeature::NumFlags(flags) => {
                            tk.underscore_mut().num_flags = flags.clone();
                        }
                    }
                    first_in_span = false;
                }
            }
        }

        let mut tokens: Vec<MToken> =
            tokens_with_spans.into_iter().map(|(tk, _)| tk).collect();

        // Tag at WORD level, as misaki does. spaCy tags its own tokens and
        // misaki's subtokens inherit the parent's tag; our tokenizer splits
        // finer than spaCy's, so tagging subtokens asks the model about
        // fragments it never saw in training ("Mr" and "." separately rather
        // than "Mr."). Group by adjacency first -- the same grouping that
        // reassembles words later -- tag the group texts, and give every
        // subtoken its group's tag.
        //
        // norm is the lowercased text. spaCy also applies a norm-exceptions
        // table through its tokenizer ("n't" -> "not"), which only fires on
        // pieces its own tokenizer produces; ours does not produce them, and
        // the group text ("hadn't") is what gets tagged.
        // misaki's pipeline, now reproduced rather than approximated:
        //
        //     doc = self.nlp(text)                  # spaCy tokenize + tag
        //     mutable_tokens = [MToken(text=tk.text, tag=tk.tag_, ...) for tk in doc]
        //     ... retokenize: subtokenize(token.text), each subtoken keeping the
        //         parent's tag, the LAST one keeping the parent's whitespace
        //
        // The grouping rule downstream is `tk.tag in PUNCT_TAGS` on a spaCy
        // token, so the tags have to come from spaCy's tokenization and not
        // from ours. Both halves are now exact against spaCy (771/771
        // sentences for the tokenizer, 20623/20623 tokens for the tagger), so
        // the approximations that used to live here -- merging abbreviations
        // and apostrophe pieces to guess at spaCy's boundaries -- are gone.
        let spacy_tokens = self.spacy_tokenizer.tokenize(&processed_text);
        let tagger_input: Vec<(String, String, bool)> = spacy_tokens
            .iter()
            .map(|tok| {
                let (t, ws) = (&tok.text, &tok.whitespace);
                // The NORM the tokenizer RULE supplied wins. It has to: `you’d`
                // gives its second piece the norm `'d` while `you’d’ve` gives
                // the same orth `would`, so no table keyed on the orth alone is
                // right, and with the wrong norm the tagger called that piece
                // an MD where spaCy says '' -- which decides whether the word
                // stays joined.
                let norm = tok
                    .norm
                    .clone()
                    .unwrap_or_else(|| crate::norms::norm_of(t, crate::spacy_tagger::hash_string));
                (t.clone(), norm, !ws.is_empty())
            })
            .collect();
        let group_tags = self.spacy_tagger.tag(&tagger_input);
        // Subtokenize each spaCy token with misaki's SUBTOKEN_REGEX. Every
        // subtoken inherits the parent's tag; only the last carries the
        // parent's trailing whitespace (`tks[-1].whitespace = token.whitespace`).
        //
        // Link markers are applied here, by BYTE SPAN against the
        // preprocessed text. Carrying them across by matching token text was
        // fragile and lost `[hello](-1)`: the same word appears more than once
        // and the first match wins.
        //
        // Spans are exact because the tokens concatenate back to the text --
        // each is `token + trailing whitespace`, in order.
        let mut rebuilt: Vec<MToken> = Vec::with_capacity(tokens.len());
        let mut tags: Vec<String> = Vec::new();
        let mut pos = 0usize;
        for (si, st) in spacy_tokens.iter().enumerate() {
            let (stext, sws) = (&st.text, &st.whitespace);
            let tag = group_tags.get(si).copied().unwrap_or("NN").to_string();
            let subs: Vec<String> = self
                .subtoken_regex
                .find_iter(stext)
                .flatten()
                .map(|m| m.as_str().to_string())
                .collect();
            let subs = if subs.is_empty() { vec![stext.clone()] } else { subs };
            let n = subs.len();
            let subs_snapshot = subs.clone();
            for (j, sub) in subs.into_iter().enumerate() {
                let start = pos;
                let end = pos + sub.len();
                pos = end;
                let mut tk = MToken::new(sub, tag.clone(), String::new());
                if j + 1 == n {
                    tk.whitespace = sws.clone();
                    pos += sws.len();
                }
                // misaki's `2` -> `to` alias (`retokenize`):
                //
                //     elif 0 < j < len(tks)-1 and tk.text == '2' \
                //          and (tks[j-1].text[-1]+tks[j+1].text[0]).isalpha():
                //         tk._.alias = 'to'
                //
                // A `2` strictly inside a word with letters either side is read
                // as "to", which is what makes `G2P` ʤˈitəpˈi. The alias also
                // makes the token stand alone, since misaki's first grouping
                // branch takes any token with an alias or phonemes.
                if j > 0 && j + 1 < n && tk.text == "2" {
                    let left_ok = subs_snapshot
                        .get(j - 1)
                        .and_then(|t: &String| t.chars().last())
                        .map(|c| c.is_alphabetic())
                        .unwrap_or(false);
                    let right_ok = subs_snapshot
                        .get(j + 1)
                        .and_then(|t: &String| t.chars().next())
                        .map(|c| c.is_alphabetic())
                        .unwrap_or(false);
                    if left_ok && right_ok {
                        tk.underscore_mut().alias = Some("to".to_string());
                    }
                }

                let mut first_in_span = true;
                for marker in &markers {
                    if start >= marker.byte_start && end <= marker.byte_end {
                        match &marker.feature {
                            LinkFeature::Stress(st) => {
                                tk.underscore_mut().stress = Some(*st);
                            }
                            LinkFeature::Phonemes(ph) => {
                                // Only the head subtoken takes the override;
                                // the rest are blanked so the loop skips them
                                // (`phonemes = v.lstrip('/') if i == 0 else ''`).
                                tk.phonemes =
                                    Some(if first_in_span { ph.clone() } else { String::new() });
                                first_in_span = false;
                            }
                            LinkFeature::NumFlags(f) => {
                                tk.underscore_mut().num_flags = f.clone();
                            }
                        }
                    }
                }
                rebuilt.push(tk);
                tags.push(tag.clone());
            }
        }
        tokens = rebuilt;

        log::debug!(
            "g2p '{}' -> {} tokens, {} tags",
            text,
            tokens.len(),
            tags.len()
        );
        for (i, tk) in tokens.iter().enumerate() {
            log::debug!("token[{}]: '{}'", i, tk.text);
        }

        // Process tokens in reverse order (like Python) to build context
        let mut contexts: Vec<crate::lexicon::TokenContext> =
            vec![crate::lexicon::TokenContext::default(); tokens.len()];

        // First, set tags
        for (tk, tag) in tokens.iter_mut().zip(tags.iter()) {
            tk.tag = tag.clone();
        }

        // `is_head` per subtoken. misaki sets `tk._.is_head = False` on any
        // subtoken that JOINS an open group, and `get_number` branches on it:
        // a three-digit group that is not the head reads as digit-then-pair
        // rather than hundreds, so the 555 of a phone number is "five fifty
        // five" and not "five hundred fifty five".
        let head_groups = Self::group_by_adjacency(&tokens);
        let mut is_head: Vec<bool> = vec![false; tokens.len()];
        for g in &head_groups {
            if g.start < is_head.len() {
                is_head[g.start] = true;
            }
        }

        // misaki's currency state machine (`retokenize`):
        //
        //     elif tk.tag == '$' and tk.text in CURRENCIES:
        //         currency = tk.text; tk.phonemes = ''; tk._.rating = 4
        //     ...
        //     elif j+1 == len(tks) and (i+1 == len(tokens) or tokens[i+1].tag != 'CD'):
        //         tk._.currency = currency
        //
        // The symbol becomes its own silent token and the unit is spoken by the
        // number after it: `$9` is [('$', ''), ('9', 'nˈIn dˈɑləɹz')]. Measured,
        // it reaches only the IMMEDIATELY following number -- `$9 and 5 more`
        // leaves the 5 bare.
        //
        // This has to be a separate pass because spaCy tokenizes the symbol
        // away from the digits, so the two are never in one group.
        let mut currency_of: Vec<Option<char>> = vec![None; tokens.len()];
        {
            let mut pending: Option<char> = None;
            for i in 0..tokens.len() {
                let c1 = {
                    let t = &tokens[i].text;
                    if t.chars().count() == 1 { t.chars().next() } else { None }
                };
                if tokens[i].tag == "$" {
                    if let Some(c) = c1 {
                        if matches!(c, '$' | '\u{a3}' | '\u{20ac}') {
                            pending = Some(c);
                            tokens[i].phonemes = Some(String::new());
                            continue;
                        }
                    }
                }
                if let Some(c) = pending.take() {
                    currency_of[i] = Some(c);
                }
            }
        }

        // misaki assigns punctuation its phonemes in `retokenize`, i.e. BEFORE
        // any lexicon lookup, and the resolution loop then skips those tokens
        // because `tk.phonemes is not None`. Order matters: our lookup
        // normalises ‘ to a straight quote, which the lexicon resolves to ”, so
        // reaching the lexicon at all gave ‘ the CLOSING glyph.
        //
        // The test is misaki's own, on the TAG. A character test was wrong in
        // both directions: it gave `[` an opening paren where spaCy tags it XX
        // rather than -LRB-, so misaki merges it into the following word and it
        // contributes NOTHING -- `[Illustration` is ˌɪləstrˈAʃən, not
        // `( ˌɪləstrˈAʃən`.
        for i in 0..tokens.len() {
            if tokens[i].phonemes.is_some() || tokens[i].text.is_empty() {
                continue;
            }
            // misaki's dash branch comes BEFORE the general punctuation one
            // (`retokenize`):
            //
            // ```text
            //     elif tk.tag == ':' and tk.text in ('-', '–'):
            //         tk.phonemes = '—'
            // ```
            //
            // A hyphen or en dash that spaCy tagged ':' is spoken as an EM
            // dash. The general path drops it instead, since '-' is not in
            // PUNCTS -- so `laws-a-me` lost its dashes and `1-25—1469-94` ran
            // its first two numbers together.
            if tokens[i].tag == ":" && (tokens[i].text == "-" || tokens[i].text == "\u{2013}") {
                tokens[i].phonemes = Some("\u{2014}".to_string());
                continue;
            }
            if Self::is_punct_token(&tokens[i]) {
                let tg = tokens[i].tag.clone();
                tokens[i].phonemes = Some(Self::punct_phonemes(&tokens[i].text, &tg));
            }
        }

        // Process in reverse to build context from future tokens
        for i in (0..tokens.len()).rev() {
            // misaki normalises the LOOKUP KEY before touching the lexicon
            // (`Lexicon.__call__`):
            //
            //     word = tk.text.replace(chr(8216), "'").replace(chr(8217), "'")
            //     word = unicodedata.normalize('NFKC', word)
            //
            // The lexicons are keyed with STRAIGHT apostrophes -- "it's" is in
            // us_gold as ɪts, "I'm" as ˌIm, "'s" as s -- so prose using curly
            // ’ missed every one of them and fell through to spelling the
            // pieces out. The token's own text keeps the original character.
            // misaki looks up `tk._.alias` in place of the text when one is
            // set (`word = tk.text if tk._.alias is None else tk._.alias`), and
            // it happens HERE rather than in a pre-pass so the lookup sees the
            // rolling context: `to` is tə before a consonant and tu before a
            // vowel, and in `G2P` the `P` that follows is a consonant.
            let word = match tokens[i].underscore().alias.clone() {
                Some(a) => a,
                None => Self::normalize_lookup(&tokens[i].text),
            };
            let tag = tokens[i].tag.clone();
            // An explicit `[word](+N)` marker takes precedence over
            // capitalization-derived stress, matching Python misaki which
            // writes `tk._.stress = v` before the lexicon lookup runs.
            let stress = tokens[i].underscore().stress.or_else(|| {
                if word == word.to_lowercase() {
                    None
                } else if word == word.to_uppercase() {
                    Some(self.lexicon.cap_stresses.1)
                } else {
                    Some(self.lexicon.cap_stresses.0)
                }
            });

            // NOTE: `contexts[i]` was already filled by iteration i+1 from that
            // token's RESOLVED PHONEMES -- see the `token_context` call at the
            // end of this loop. There was an orthographic heuristic here
            // ("does the next word start with aeiou") that overwrote it on
            // every iteration, which is why `the FBI` came out `\u00f0\u0259` instead of
            // `\u00f0i`: F is not in "aeiou", but FBI's phonemes begin `\u02cc\u025b`, a vowel.
            // misaki decides this from phonemes, never from spelling.

            // Process current token
            if tokens[i].phonemes.is_none() {
                let ctx = Some(&contexts[i]);

                // Use get_word which handles special cases, lookup, and stemming
                if let Some((ps, _)) = self.lexicon.get_word(&word, &tag, stress, ctx) {
                    tokens[i].phonemes = Some(ps);
                }

                if tokens[i].phonemes.is_none() {
                    // The number path comes FIRST. misaki has no
                    // hyphen-splitting branch at all -- it is an invention of
                    // this port -- and letting it win meant a signed number was
                    // split on its own sign: `-40` became "" + "forty" and the
                    // minus was simply lost, where misaki says mˈInəs fˈɔɹTi.
                    if self.is_number_like(&word) && word.starts_with('-') {
                        let spoken = self.convert_number(&word);
                        if spoken != word {
                            if let Some(p) = self.number_phonemes(&spoken) {
                                tokens[i].phonemes = Some(p);
                            } else if let Ok((p, _)) = self.g2p(&spoken) {
                                tokens[i].phonemes = Some(p.trim_end().to_string());
                            }
                        }
                    } else if word.contains('-') && word.len() > 1 {
                        // Handle hyphenated words like "twenty-one"
                        let parts: Vec<&str> = word.split('-').filter(|s| !s.is_empty()).collect();
                        let mut sub_ps = Vec::new();
                        for part in parts {
                            let (p, _) = self.g2p(part)?;
                            sub_ps.push(p);
                        }
                        tokens[i].phonemes = Some(sub_ps.join(" "));
                    } else if let Some(spoken) = currency_of[i]
                        .filter(|_| {
                            let c = word.replace(',', "");
                            !c.is_empty() && c.chars().all(|ch| ch.is_ascii_digit() || ch == '.')
                        })
                        .and_then(|sym| self.currency_words(sym, &word))
                    {
                        // A number carrying a currency from the symbol before
                        // it. This is handled here as well as in the merge pass
                        // because spaCy puts the symbol in its own token, so a
                        // bare `$9` never forms a multi-token group at all.
                        if let Some(p) = self.number_phonemes(&spoken) {
                            tokens[i].phonemes = Some(p);
                        } else if let Ok((p, _)) = self.g2p(&spoken) {
                            tokens[i].phonemes = Some(p.trim_end().to_string());
                        }
                    } else if let Some(spoken) = (!is_head[i])
                        .then(|| self.nonhead_number(&word))
                        .flatten()
                    {
                        // A numeric group that is not the head of its word.
                        if let Some(p) = self.number_phonemes(&spoken) {
                            tokens[i].phonemes = Some(p);
                        } else if let Ok((p, _)) = self.g2p(&spoken) {
                            tokens[i].phonemes = Some(p.trim_end().to_string());
                        }
                    } else if self.is_number_like(&word) {
                        let spoken = self.convert_number(&word);
                        if spoken != word {
                            // Word by word, so "point" keeps its -2 stress.
                            if let Some(p) = self.number_phonemes(&spoken) {
                                tokens[i].phonemes = Some(p);
                            } else {
                                let (p, _) = self.g2p(&spoken)?;
                                tokens[i].phonemes = Some(p.trim_end().to_string());
                            }
                        }
                    }
                }

                if tokens[i].phonemes.is_none() {
                    if let Some(ps) = self.rules.apply_rules(&word, &tag, &self.lexicon) {
                        tokens[i].phonemes = Some(ps);
                    }
                }

                if tokens[i].phonemes.is_none() {
                    if word.chars().count() > 1 {
                        // Unknown multi-character word - use fallback
                        let mut handled = false;
                        if let Some(ref fallback) = self.fallback {
                            match fallback.phonemize(&word) {
                                Ok(ps) => {
                                    tokens[i].phonemes = Some(ps);
                                    handled = true;
                                }
                                // misaki's fallback answers (None, None) for nothing spoken
                                Err(FallbackError::NoPhonemes { .. }) => {}
                                Err(e) => {
                                    log::error!("fallback error for '{}': {}", word, e);
                                    return Err(G2PError::Fallback(e));
                                }
                            }
                        }

                        if !handled {
                            // Leave it unresolved. misaki returns (None, None) for an
                            // out-of-vocabulary word (`Lexicon.__call__`); with no
                            // fallback configured the phonemes stay None and the
                            // assembled string substitutes `unk`.
                            //
                            // There was a character-by-character spell-out here. It is
                            // not what misaki does and it was actively harmful: every
                            // unknown proper noun became its spelling, so "Bennet" read
                            // as "b-e-n-n-e-t" and one token became six, which is why
                            // 257 of 413 failing prose cases differed in TOKEN COUNT
                            // and therefore in every downstream word timestamp.
                            let _ = &mut handled;
                        }
                    } else {
                        // Try to normalize the character or return unknown
                        let normalized: String = word
                            .chars()
                            .map(|c| match c {
                                'é' | 'è' | 'ê' | 'ë' => 'e',
                                'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' => 'a',
                                'í' | 'ì' | 'î' | 'ï' => 'i',
                                'ó' | 'ò' | 'ô' | 'ö' | 'õ' => 'o',
                                'ú' | 'ù' | 'û' | 'ü' => 'u',
                                'ñ' => 'n',
                                'ç' => 'c',
                                // NB: em/en dashes are NOT mapped away here.
                                // U+2014 is in misaki's PUNCTS and survives as
                                // itself; mapping it to a space sent it down the
                                // renormalise path and silently dropped it.
                                _ => c,
                            })
                            .collect();

                        if normalized != word {
                            let (p, _) = self.g2p(&normalized)?;
                            tokens[i].phonemes = Some(p);
                        } else {
                            // Punctuation, as misaki resolves it (`en.py:598-599`):
                            //
                            //     elif tk.tag in PUNCT_TAGS and not all(97 <= ord(c.lower()) <= 122 ...):
                            //         tk.phonemes = PUNCT_TAG_PHONEMES.get(
                            //             tk.tag, ''.join(c for c in tk.text if c in PUNCTS))
                            //
                            // with PUNCTS = ';:,.!?\u2014\u2026"\u201c\u201d' and PUNCT_TAG_PHONEMES
                            // mapping spaCy's quote/paren tags: `` -> \u201c, '' -> \u201d,
                            // -LRB- -> (, -RRB- -> ).
                            //
                            // The punctuation pre-pass above already handled
                            // every token misaki calls punctuation, by TAG. A
                            // non-alphanumeric token that gets here is therefore
                            // one spaCy did NOT tag as punctuation -- `[` is
                            // tagged XX, not -LRB- -- and misaki leaves it
                            // unresolved, so it contributes nothing when merged
                            // into the following word. `[Illustration` is
                            // ˌɪləstrˈAʃən, not `( ˌɪləstrˈAʃən`.
                            //
                            // SUBTOKEN_JUNKS still resolve to the empty string
                            // (resolve_tokens), which the merge pass applies.
                            tokens[i].phonemes = None;
                        }
                    }
                }
            }

            // Propagate context leftwards, exactly as misaki's
            // `G2P.token_context` does (`en.py`):
            //
            //     vowel = next((None if c in NON_QUOTE_PUNCTS else (c in VOWELS)
            //                   for c in ps if any(c in s for s in
            //                   (VOWELS, CONSONANTS, NON_QUOTE_PUNCTS))), vowel) if ps else vowel
            //
            // Three things the previous version got wrong and this one does not:
            // punctuation RESETS future_vowel to None rather than being skipped;
            // a token whose phonemes contain no vowel, consonant or punctuation
            // CARRIES THE PREVIOUS value rather than leaving a stale one; and
            // future_to is set here from the token, not guessed from spelling.
            if i > 0 {
                contexts[i - 1] =
                    Self::token_context(&contexts[i], tokens[i].phonemes.as_deref(), &tokens[i]);
            }
        }

        // ---- merge adjacent subtokens into words (misaki's `merge_tokens`) ----
        //
        // misaki's final token list has ONE token per adjacency group:
        //
        //     tokens = [merge_tokens(tk, unk=self.unk) if isinstance(tk, list) else tk
        //               for tk in tokens]
        //
        // and this runs BEFORE the flap pass below, which is why it is placed
        // here. Without it our tokenizer's finer splits reached the output and
        // 257 of 700 prose cases differed in token count -- every one of which
        // corrupts the word timestamps, since `pred_dur` is per token.
        //
        // Phonemes are joined with no separator unless `prespace`, which misaki
        // derives in `resolve_tokens` from the group's text:
        //
        //     prespace = ' ' in text or '/' in text or len({0 if c.isalpha()
        //                else (1 if is_digit(c) else 2) for c in text
        //                if c not in SUBTOKEN_JUNKS}) > 1
        //
        // i.e. a space, a slash, or a MIX of character classes once the junk
        // characters are ignored. `five-storied` is all-alpha once `-` is
        // dropped, so it joins tight: fˌIv + stˈɔɹid -> fˌIvstˈɔɹid.
        {
            const SUBTOKEN_JUNKS: &str = "',-._\u{2018}\u{2019}/";
            let groups = Self::group_by_adjacency(&tokens);
            if groups.len() != tokens.len() {
                let mut merged: Vec<MToken> = Vec::with_capacity(groups.len());
                for g in groups {
                    if g.len() == 1 {
                        merged.push(tokens[g.start].clone());
                        continue;
                    }
                    let group = &tokens[g.clone()];
                    let (text, tag, ws) = Self::merge_text_tag(group);

                    // misaki's `resolve_tokens`: a subtoken made entirely of
                    // SUBTOKEN_JUNKS contributes NOTHING.
                    //
                    //     elif all(c in SUBTOKEN_JUNKS for c in tk.text):
                    //         tk.phonemes = ''
                    //
                    // Without this the word-internal ’ in `it’s` kept the ”
                    // that the standalone-quote path gives it, producing
                    // ɪt”ˈɛs instead of ɪts.
                    let part_of = |tk: &MToken| -> String {
                        if !tk.text.is_empty() && tk.text.chars().all(|c| SUBTOKEN_JUNKS.contains(c)) {
                            String::new()
                        } else {
                            tk.phonemes.clone().unwrap_or_default()
                        }
                    };

                    let classes: std::collections::BTreeSet<u8> = text
                        .chars()
                        .filter(|c| !SUBTOKEN_JUNKS.contains(*c))
                        .map(|c| if c.is_alphabetic() { 0u8 } else if c.is_ascii_digit() { 1 } else { 2 })
                        .collect();
                    let prespace =
                        text.contains(' ') || text.contains('/') || classes.len() > 1;

                    // misaki tries the whole word in the lexicon first and only
                    // falls back to stitching the parts together.
                    // misaki's multi-token loop (`G2P.__call__`). It does not
                    // pick one span -- it resolves the word in PIECES, right to
                    // left, and the result is their concatenation:
                    //
                    //     left, right = 0, len(w)
                    //     while left < right:
                    //         tk = merge_tokens(w[left:right])      # if none resolved yet
                    //         ps, rating = self.lexicon(tk, ctx)
                    //         if ps is not None:
                    //             w[left].phonemes = ps
                    //             for x in w[left+1:right]: x.phonemes = ''
                    //             right = left; left = 0
                    //         elif left + 1 < right: left += 1
                    //         else:
                    //             right -= 1
                    //             if all(c in SUBTOKEN_JUNKS for c in w[right].text):
                    //                 w[right].phonemes = ''
                    //             left = 0
                    //
                    // `Austen’s` needs this: "Austen's" misses, then the span
                    // "'s" HITS (us_gold holds "'s" as s), and Austen stays
                    // unresolved -- giving misaki's `s`. Trying only the whole
                    // word stitched on the letter name ˈɛs instead.
                    // misaki's `Lexicon.__call__` tries get_word, then the
                    // NUMBER path, before anything else:
                    //
                    //     ps, rating = self.get_word(...)
                    //     if ps is not None: return ...
                    //     elif Lexicon.is_number(word, tk._.is_head):
                    //         ps, rating = self.get_number(...)
                    //
                    // Our tokenizer splits `1st` into `1` + `st`, so the group
                    // text is the first point at which it looks like an ordinal
                    // at all. Without this `1st` read as "one" and `23rd` as
                    // "twenty three".
                    // Currency. misaki's `Lexicon.get_number` pairs the
                    // dot-separated parts with CURRENCIES[sym] -- $ is
                    // (dollar, cent), £ is (pound, pence), € is (euro, cent) --
                    // drops a zero part, joins the rest with "and", and uses the
                    // singular at exactly one:
                    //
                    //   $9        -> nine dollars
                    //   $1        -> one dollar
                    //   $0.50     -> fifty cents        (the zero major is dropped)
                    //   $1,250.75 -> one thousand two hundred fifty dollars
                    //                and seventy five cents
                    if let Some(spoken) = currency_of[g.start]
                        .filter(|_| {
                            let c = text.replace(',', "");
                            !c.is_empty() && c.chars().all(|ch| ch.is_ascii_digit() || ch == '.')
                        })
                        .and_then(|sym| self.currency_words(sym, &text))
                    {
                        if let Ok((p, _)) = self.g2p(&spoken) {
                            // The symbol is already its own token, silenced by
                            // the pass above, so only the number is emitted here.
                            let mut tk = group[0].clone();
                            tk.text = text;
                            tk.tag = tag;
                            tk.whitespace = ws;
                            tk.phonemes = Some(p);
                            merged.push(tk);
                            continue;
                        }
                    }

                    if self.is_number_like(&text) {
                        let spoken = self.convert_number(&text);
                        if spoken != text {
                            // Word by word, so "point" keeps its -2 stress; the
                            // phrase path is only a fallback.
                            if let Some(p) = self.number_phonemes(&spoken).or_else(|| {
                                self.g2p(&spoken).ok().map(|(p, _)| p.trim_end().to_string())
                            }) {
                                let mut tk = group[0].clone();
                                tk.text = text;
                                tk.tag = tag;
                                tk.whitespace = ws;
                                tk.phonemes = Some(p);
                                merged.push(tk);
                                continue;
                            }
                        }
                    }

                    let n = g.len();
                    let mut res: Vec<Option<String>> = vec![None; n];
                    let mut left = 0usize;
                    let mut right = n;
                    let mut should_fallback = false;
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
                        (Some(fallback), true) => match fallback.phonemize(&text) {
                            Err(FallbackError::NoPhonemes { .. }) => None,
                            other => Some(other?),
                        },
                        _ => None,
                    };

                    let phonemes = whole.or_else(|| {
                        // misaki's `resolve_tokens` rebalances stress across the
                        // parts of one word before they are joined:
                        //
                        //     indices = [(PRIMARY_STRESS in tk.phonemes,
                        //                 stress_weight(tk.phonemes), i) ...]
                        //     if len(indices) == 2 and len(tokens[indices[0][2]].text) == 1:
                        //         demote indices[1]
                        //     elif len(indices) < 2 or sum(b for b,_,_ in indices) <= (len(indices)+1)//2:
                        //         return
                        //     for _,_,i in sorted(indices)[:len(indices)//2]: demote i
                        //
                        // where demote is `apply_stress(ps, -0.5)` -- primary
                        // becomes secondary. Without it a compound kept a primary
                        // stress on every part: `Caucus-Race` came out
                        // kˈɔkəsɹˈAs where misaki gives kˈɔkəsɹˌAs.
                        //
                        // `stress_weight` counts diphthongs double, which is what
                        // decides WHICH part is demoted: ɹˈAs weighs 5 (A is a
                        // diphthong) against kˈɔkəs's 6, so Race loses its primary.
                        let mut parts: Vec<String> = res
                            .iter()
                            .enumerate()
                            .map(|(i, r)| match r {
                                Some(ps) => ps.clone(),
                                // Unresolved: misaki writes `unk`, which our
                                // pipeline sets to the empty string. Fall back to
                                // whatever the per-token pass found only for a
                                // token the span loop never reached.
                                None => part_of(&group[i]),
                            })
                            .collect();
                        // `resolve_tokens` RETURNS before rebalancing when the
                        // parts are space-separated:
                        //
                        //     if prespace:
                        //         return
                        //
                        // so a mixed-class word keeps every primary stress.
                        // That is why misaki gives `50%` as fˈɪfti pəɹsˈɛnt and not
                        // fˌɪfti pəɹsˈɛnt -- '5','0' and '%' are different classes,
                        // prespace is true, and no demotion happens.
                        if !prespace {
                            const PRIMARY: char = '\u{2c8}';
                            const DIPHTHONGS: &str = "AIOQWY\u{2a4}\u{2a7}";
                            let weight = |ps: &str| -> usize {
                                ps.chars()
                                    .map(|c| if DIPHTHONGS.contains(c) { 2 } else { 1 })
                                    .sum()
                            };
                            let mut idx: Vec<(bool, usize, usize)> = parts
                                .iter()
                                .enumerate()
                                .filter(|(_, p)| !p.is_empty())
                                .map(|(i, p)| (p.contains(PRIMARY), weight(p), i))
                                .collect();
                            let n_primary = idx.iter().filter(|(b, _, _)| *b).count();
                            if idx.len() == 2
                                && group[idx[0].2].text.chars().count() == 1
                            {
                                let i = idx[1].2;
                                parts[i] = self.lexicon.apply_stress(&parts[i], Some(-0.5));
                            } else if idx.len() >= 2 && n_primary > (idx.len() + 1) / 2 {
                                idx.sort();
                                let half = idx.len() / 2;
                                for (_, _, i) in idx.into_iter().take(half) {
                                    parts[i] = self.lexicon.apply_stress(&parts[i], Some(-0.5));
                                }
                            }
                        }

                        let mut out = String::new();
                        for part in parts {
                            if prespace
                                && !out.is_empty()
                                && !out.ends_with(char::is_whitespace)
                                && !part.is_empty()
                            {
                                out.push(' ');
                            }
                            out.push_str(&part);
                        }
                        Some(out)
                    });

                    let mut tk = group[0].clone();
                    tk.text = text;
                    tk.tag = tag;
                    tk.whitespace = ws;
                    tk.phonemes = phonemes;
                    merged.push(tk);
                }
                tokens = merged;
            }
        }

        // ---- contexts from the MERGED words, then re-resolve ----
        //
        // misaki's resolution loop iterates over adjacency GROUPS, so the
        // lookahead context it carries leftward comes from the phonemes of a
        // whole word. Ours resolves subtokens first and merges afterwards, so
        // the context came from a SUBTOKEN, and a word whose merged phonemes
        // differ from its parts' gave the wrong answer:
        //
        // ```text
        //     "I belong to Tellson’s Bank."
        //       misaki  to -> tə    (merged `Tellson’s` is `s`, a consonant)
        //       ours    to -> tʊ    (the `s` SUBTOKEN is the letter name ˈɛs)
        // ```
        //
        // Rather than restructure the loop, the contexts are recomputed here
        // over the finished word list -- which is exactly what misaki's loop
        // sees -- and the words whose pronunciation depends on that context are
        // resolved again against it.
        //
        // The re-resolution is deliberately narrow. It touches a token only
        // when it is a plain alphabetic word with no alias, already resolved by
        // the lexicon, and `get_word` under the new context returns something
        // else. Numbers, currency, punctuation, aliases and out-of-vocabulary
        // words are left exactly as they were.
        {
            let n = tokens.len();
            let mut ctxs = vec![crate::lexicon::TokenContext::default(); n];
            let mut rolling = crate::lexicon::TokenContext::default();
            for i in (0..n).rev() {
                ctxs[i] = rolling.clone();
                rolling =
                    Self::token_context(&rolling, tokens[i].phonemes.as_deref(), &tokens[i]);
            }
            for i in 0..n {
                if tokens[i].underscore().alias.is_some() {
                    continue;
                }
                let text = tokens[i].text.clone();
                if text.is_empty() || !text.chars().all(|c| c.is_alphabetic()) {
                    continue;
                }
                match tokens[i].phonemes.as_deref() {
                    Some(p) if !p.is_empty() => {}
                    _ => continue,
                }
                let key = Self::normalize_lookup(&text);
                let stress = tokens[i].underscore().stress.or_else(|| {
                    if key == key.to_lowercase() {
                        None
                    } else if key == key.to_uppercase() {
                        Some(self.lexicon.cap_stresses.1)
                    } else {
                        Some(self.lexicon.cap_stresses.0)
                    }
                });
                let tag = tokens[i].tag.clone();
                if let Some((ps, _)) =
                    self.lexicon.get_word(&key, &tag, stress, Some(&ctxs[i]))
                {
                    tokens[i].phonemes = Some(ps);
                }
            }
        }

        // misaki applies exactly this, after all resolution and merging and
        // before assembling the string (`en.py:708-710`, gated on
        // `version != '2.0'`):
        //
        //     tk.phonemes = tk.phonemes.replace('\u027e', 'T').replace('\u0294', 't')
        //
        // The lexicons store \u027e and \u0294; Kokoro's vocab wants T and t. It runs
        // AFTER resolution on purpose -- the context pass matches \u027e against its
        // consonant set, so replacing earlier would break that -- and it is
        // applied to the TOKENS, not just the assembled string, because
        // `pred_dur` has one entry per token and the word timestamps are
        // derived from the per-token phonemes.
        for tk in tokens.iter_mut() {
            if let Some(ps) = tk.phonemes.as_mut() {
                if !ps.is_empty() {
                    *ps = ps.replace('\u{27e}', "T").replace('\u{294}', "t");
                }
            }
        }

        let result = tokens
            .iter()
            .map(|tk| tk.phonemes.as_ref().unwrap_or(&self.unk).clone() + &tk.whitespace)
            .collect::<String>();

        Ok((result, tokens))
    }

}

#[cfg(test)]
mod tests;
