# misaki-rs

English text to Kokoro's phonemes in Rust, with the same output as Python's [misaki](https://github.com/hexgrad/misaki) 0.9.4, the version Kokoro v1.0 was trained with.

This is a fork of [MicheleYin/misaki-rs](https://github.com/MicheleYin/misaki-rs) built on [durgaprasadksv/misaki-rs](https://github.com/durgaprasadksv/misaki-rs). That fork matches misaki byte for byte without eSpeak (see [PARITY.md](PARITY.md)). This one also matches misaki's eSpeak fallback, which reads out words missing from the dictionary.

Measured against Python misaki 0.9.4 with its eSpeak fallback, on 3,592 passages from novels and on the 44,489 distinct words in them:

| | Words the same |
|---|---|
| misaki-rs 0.6.0 (crates.io) | 40.8 % |
| durgaprasadksv's fork | 98.0 % |
| this fork | 99.9 % (53 words differ, mostly around dashes and quotes) |

## Install

Paste this into Claude Code:

```
Add misaki-rs from https://github.com/elana-voss/misaki-rs to this Rust project and phonemize a test sentence with it.
```

Or by hand, in `Cargo.toml`:

```toml
misaki-rs = { git = "https://github.com/elana-voss/misaki-rs" }
```

```rust
use misaki_rs::{G2P, Language};

let g2p = G2P::new(Language::EnglishUS);
let (phonemes, _tokens) = g2p.g2p("“I ask your favour,” she said.")?;
// “ˌI ˈæsk jʊɹ fˈAvəɹ,” ʃi sˈɛd.
```

`Language::EnglishGB` gives British English. `G2P::with_fallback` takes your own `Fallback` for unknown words in place of eSpeak.

## eSpeak

The `espeak` feature, on by default, builds espeak-ng in. Its data folder `espeak-ng-data` is looked for in `PIPER_ESPEAKNG_DATA_DIRECTORY`, the current folder and the program's folder, else in an installed eSpeak NG. Without the feature, unknown words come out as `❓`, as misaki does without a fallback.

On Windows with MSVC, espeak-rs-sys 0.1.9 needs two changes to build: `USE_ASYNC` off (there is no `pthread.h`) and `advapi32` linked. Patch it in your `Cargo.toml` with `[patch.crates-io]`.

## What this fork changed

- The eSpeak fallback asks espeak-ng for its phonemes with ties, as misaki does through phonemizer, and maps them to Kokoro's symbols with misaki's table. Before, the sounds stayed in eSpeak's form and every such word ended in a full stop.
- A word that is partly unknown, like `Gor's`, goes to eSpeak whole, as misaki does. Before, eSpeak read `Gor` and the dictionary's `'s` was stuck on, giving `s` where misaki says `z`.

## License

MIT. With the `espeak` feature the program includes espeak-ng, which is GPL-3.0. The dictionaries come from misaki (Apache-2.0).
