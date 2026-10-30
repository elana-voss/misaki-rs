//! Numbers, years and currency as words (misaki's `Lexicon.get_number`).
use super::*;

impl G2P {
    /// Does this token go down misaki's number path?
    ///
    /// misaki's `Lexicon.is_number` peels an alphabetic suffix first, so `1st`,
    /// `23rd` and `2nd` are numbers, not words. Testing only for all-digits
    /// meant an ordinal never reached `get_number` and `1st` read as "one".
    pub(super) fn is_number_like(&self, word: &str) -> bool {
        let clean = word.replace(',', "");
        let lower = clean.to_lowercase();
        for suf in ["st", "nd", "rd", "th"] {
            if let Some(d) = lower.strip_suffix(suf) {
                if !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()) {
                    return true;
                }
            }
        }
        if let Some(rest) = clean.strip_prefix('-') {
            if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
                return true;
            }
        }
        // A single decimal point, digits on both sides.
        if clean.matches('.').count() == 1 {
            if let Some((a, b)) = clean.split_once('.') {
                if !a.is_empty()
                    && !b.is_empty()
                    && a.chars().all(|c| c.is_ascii_digit())
                    && b.chars().all(|c| c.is_ascii_digit())
                {
                    return true;
                }
            }
        }
        self.is_number(word)
    }

    pub(super) fn is_number(&self, word: &str) -> bool {
        let clean = word.replace(",", "");
        clean.parse::<i64>().is_ok()
    }

    /// Reduce a num2words string to the word sequence misaki feeds the lexicon.
    ///
    /// misaki does `re.split(r'[^a-z]+', num2words(int(num)))` and looks each
    /// piece up on its own (`Lexicon.get_number`), so hyphens and commas become
    /// word boundaries -- "fifty-seven" is two lookups, not one. It also drops
    /// "and" unless '&' is in num_flags, which is why 2001 reads
    /// "two thousand one".
    ///
    /// Without this, "457" produced `f\u02c8\u0254\u0279 h\u02c8\u028cndr\u0259d f\u02c8\u026afti s\u02c8\u025bv\u0259n` with the last
    /// two words run together, because "fifty-seven" went to the lexicon whole.
    pub(super) fn normalize_num_words(spoken: &str) -> String {
        spoken
            .split(|c: char| !c.is_ascii_alphabetic())
            .filter(|w| !w.is_empty() && *w != "and")
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// num2words' `to='year'` form, which misaki uses for any bare 4-digit
    /// number that is not a currency amount (`en.py`:
    /// `len(word) == 4 and currency not in CURRENCIES`).
    ///
    /// Verified against Python num2words: 1999 "nineteen ninety-nine",
    /// 1900 "nineteen hundred", 2000 "two thousand", 2001 "two thousand and
    /// one", 2005 "two thousand and five", 2010 "twenty ten", 1066 "ten
    /// sixty-six", 1905 "nineteen oh-five", 1100 "eleven hundred", 2100
    /// "twenty-one hundred", 1010 "ten ten".
    pub(super) fn year_words(y: i64) -> Option<String> {
        let card = |n: i64| Num2Words::new(n).to_words().ok();
        let (hi, lo) = (y / 100, y % 100);
        if lo == 0 {
            if y % 1000 == 0 {
                card(y) // 2000 -> two thousand
            } else {
                Some(format!("{} hundred", card(hi)?)) // 1900 -> nineteen hundred
            }
        } else if hi % 10 == 0 && lo < 10 {
            card(y) // 2005 -> two thousand and five
        } else if lo < 10 {
            Some(format!("{} oh {}", card(hi)?, card(lo)?)) // 1905 -> nineteen oh-five
        } else {
            Some(format!("{} {}", card(hi)?, card(lo)?)) // 1999 -> nineteen ninety-nine
        }
    }

    /// A currency amount spelled out, or None if `word` is not one.
    ///
    /// See the call site for misaki's rules; the units come from its CURRENCIES
    /// table.
    pub(super) fn currency_words(&self, sym: char, amount: &str) -> Option<String> {
        let units: (&str, &str) = match sym {
            '$' => ("dollar", "cent"),
            '\u{a3}' => ("pound", "pence"),
            '\u{20ac}' => ("euro", "cent"),
            _ => return None,
        };
        let rest: String = amount.replace(',', "");
        if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit() || c == '.') {
            return None;
        }
        let mut parts: Vec<(i64, &str)> = Vec::new();
        for (i, seg) in rest.split('.').enumerate().take(2) {
            let n: i64 = if seg.is_empty() { 0 } else { seg.parse().ok()? };
            parts.push((n, if i == 0 { units.0 } else { units.1 }));
        }
        // misaki drops a zero part rather than saying "zero dollars".
        if parts.len() > 1 {
            if parts[1].0 == 0 {
                parts.truncate(1);
            } else if parts[0].0 == 0 {
                parts.remove(0);
            }
        }
        let mut out: Vec<String> = Vec::new();
        for (n, unit) in parts {
            if !out.is_empty() {
                out.push("and".to_string());
            }
            out.push(Self::normalize_num_words(
                &Num2Words::new(n).to_words().ok()?,
            ));
            out.push(if n == 1 {
                unit.to_string()
            } else {
                format!("{unit}s")
            });
        }
        Some(out.join(" "))
    }

    /// Phonemes for a spoken number phrase, looked up word by word.
    ///
    /// misaki's `get_number` does not phonemise the phrase as text -- it looks
    /// each word up on its own, and gives "point" a stress of -2:
    ///
    /// ```text
    ///     result.append(self.lookup(w, None, -2 if w == 'point' else None, None))
    /// ```
    ///
    /// Re-running the whole phrase through g2p instead left "point" stressed,
    /// so 3.5 came out θɹˈi pˈYnt fˈIv where misaki gives θɹˈi pYnt fˈIv.
    pub(super) fn number_phonemes(&self, spoken: &str) -> Option<String> {
        let mut out: Vec<String> = Vec::new();
        for w in spoken.split_whitespace() {
            // misaki gives both "point" and the spoken-zero "O" a stress of -2.
            let stress = if w == "point" || w == "O" { Some(-2.0) } else { None };
            let (ps, _) = self.lexicon.get_word(w, "NN", stress, None)?;
            out.push(ps);
        }
        if out.is_empty() {
            None
        } else {
            Some(out.join(" "))
        }
    }

    /// A digit group as words, for the number paths that need a piece spelled
    /// without going back through the full `convert_number`.
    pub(super) fn spell(digits: &str) -> String {
        digits
            .parse::<i64>()
            .ok()
            .and_then(|n| Num2Words::new(n).to_words().ok())
            .map(|w| Self::normalize_num_words(&w))
            .unwrap_or_else(|| digits.to_string())
    }

    /// misaki's non-head reading of a digit group (`Lexicon.get_number`):
    ///
    /// ```text
    ///     elif not is_head and '.' not in word:
    ///         if num[0] == '0' or len(num) > 3: digit by digit
    ///         elif len(num) == 3 and not num.endswith('00'):
    ///             num[0], then 'O' + num[2] if num[1]=='0' else num[1:]
    ///         else: the whole as a cardinal
    /// ```
    ///
    /// Returns None when the ordinary cardinal reading applies, including for
    /// four digits -- those are caught earlier by the YEAR rule, which is why
    /// 0199 reads "one ninety nine" and 1234 "twelve thirty four".
    pub(super) fn nonhead_number(&self, word: &str) -> Option<String> {
        let num = word.replace(',', "");
        if num.is_empty() || !num.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        if num.len() == 4 {
            return None; // the year rule owns this
        }
        if num.starts_with('0') || num.len() > 3 {
            return Some(
                num.chars()
                    .map(|c| Self::spell(&c.to_string()))
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
        if num.len() == 3 && !num.ends_with("00") {
            let first = Self::spell(&num[..1]);
            let rest = &num[1..];
            return Some(if rest.starts_with('0') {
                format!("{first} O {}", Self::spell(&rest[1..]))
            } else {
                format!("{first} {}", Self::spell(rest))
            });
        }
        None
    }

    pub(super) fn convert_number(&self, word: &str) -> String {
        let clean = word.replace(",", "");

        // misaki prefixes "minus" for a leading '-' (`Lexicon.get_number`):
        //
        //     if word.startswith('-'):
        //         result.append(self.lookup('minus', ...))
        //         word = word[1:]
        //
        // We were dropping the sign, so -40 read as "forty".
        if let Some(rest) = clean.strip_prefix('-') {
            if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit() || c == '.') {
                return format!("minus {}", self.convert_number(rest));
            }
        }

        // Ordinals. misaki (`Lexicon.get_number`) peels an alphabetic suffix
        // and, when it is one of ORDINALS = {st, nd, rd, th} and the rest is
        // digits, reads the ORDINAL form:
        //
        //     if is_digit(word) and suffix in ORDINALS:
        //         extend_num(num2words(int(word), to='ordinal'), escape=True)
        //
        // Without it "23rd" read as "twenty three" and "1st" as "one".
        {
            let lower = clean.to_lowercase();
            for suf in ["st", "nd", "rd", "th"] {
                if let Some(digits) = lower.strip_suffix(suf) {
                    if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
                        if let Ok(n) = digits.parse::<i64>() {
                            if let Ok(spoken) = Num2Words::new(n).ordinal().to_words() {
                                return Self::normalize_num_words(&spoken);
                            }
                        }
                    }
                }
            }
        }

        // Bare four-digit numbers read as years, per misaki's get_number.
        if clean.len() == 4 && clean.chars().all(|c| c.is_ascii_digit()) {
            if let Ok(y) = clean.parse::<i64>() {
                if let Some(spoken) = Self::year_words(y) {
                    return Self::normalize_num_words(&spoken);
                }
            }
        }

        if let Ok(val) = clean.parse::<i64>() {
            let n2w = match self.lexicon.lang {
                Language::EnglishUS | Language::EnglishGB => Num2Words::new(val),
                // Language::Italian => Num2Words::new(val).lang(num2words::Lang::English),
            };
            if let Ok(spoken) = n2w.to_words() {
                return Self::normalize_num_words(&spoken);
            }
        }

        // A single decimal point: the integer part as a cardinal, then "point",
        // then each fractional DIGIT on its own. Verified against misaki:
        //
        //   3.5    -> three point five        3.75   -> three point seven five
        //   1.25   -> one point two five      100.01 -> one hundred point zero one
        if clean.matches('.').count() == 1 {
            let (int_part, frac) = clean.split_once('.').unwrap();
            let int_ok = !int_part.is_empty() && int_part.chars().all(|c| c.is_ascii_digit());
            let frac_ok = !frac.is_empty() && frac.chars().all(|c| c.is_ascii_digit());
            if int_ok && frac_ok {
                if let Ok(iv) = int_part.parse::<i64>() {
                    if let Ok(head) = Num2Words::new(iv).to_words() {
                        let mut out = vec![Self::normalize_num_words(&head), "point".to_string()];
                        for d in frac.chars() {
                            let dv = (d as u8 - b'0') as i64;
                            if let Ok(w) = Num2Words::new(dv).to_words() {
                                out.push(Self::normalize_num_words(&w));
                            }
                        }
                        return out.join(" ");
                    }
                }
            }
        }

        word.to_string()
    }
}
