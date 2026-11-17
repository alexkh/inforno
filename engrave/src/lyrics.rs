//! Lyrics: which syllable of a `w:` line is sung on which note.
//!
//! Every syllable takes the next note of its voice; rests and grace notes
//! take none. `_` holds the syllable before it for one more note, `*` and a
//! hyphen on its own leave a note bare, and `|` goes on after the next bar
//! line. Tied notes count one by one. The music after an `&` is a second
//! voice without words.

use crate::voices::LyricBlock;
use abc::datatypes::*;
use std::collections::HashMap;

/// What a note has under it in one verse.
#[derive(Clone, Debug, PartialEq)]
pub enum Sung {
    /// A syllable; `hyphen` when its word goes on.
    Text { text: String, hyphen: bool },
    /// `_`: the syllable before is held to this note.
    Hold,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Syllable {
    /// 0 for the first verse.
    pub verse: usize,
    pub sung: Sung,
    /// The token in the `w:` line.
    pub span: Span,
}

/// What is sung on each note, by where the note's source text starts.
pub type Underlay = HashMap<usize, Vec<Syllable>>;

enum Step {
    /// A note or chord, with the start of its source text.
    Note(usize),
    Bar,
}

/// The notes and bar lines of some music, in order.
fn steps(music: &[&Symbol]) -> Vec<Step> {
    let mut steps = vec![];
    let mut grace = false;
    // after an `&`, up to the next bar line
    let mut overlay = false;
    for sym in music {
        match &sym.kind {
            MusicSymbol::GraceStart { .. } => grace = true,
            MusicSymbol::GraceEnd => grace = false,
            MusicSymbol::Overlay => overlay = true,
            MusicSymbol::Stem { .. } if !grace && !overlay => steps.push(Step::Note(sym.span.start)),
            MusicSymbol::Bar(_) => {
                overlay = false;
                steps.push(Step::Bar);
            }
            _ => {}
        }
    }
    steps
}

/// Sets the lyrics of a voice under its notes. Lyrics that no note is left
/// for are reported.
pub fn align(blocks: &[LyricBlock], diagnostics: &mut Vec<Diagnostic>) -> Underlay {
    let mut underlay = Underlay::new();
    for block in blocks {
        let steps = steps(&block.music);
        for (verse, line) in block.verses.iter().enumerate() {
            // the next step to look at
            let mut at = 0;
            // the last syllable set, as (note, index among what the note has)
            let mut last_text: Option<(usize, usize)> = None;
            let mut left_over: Option<Span> = None;
            for token in &line.tokens {
                if token.kind == Lyric::Bar {
                    at = steps[at..]
                        .iter()
                        .position(|s| matches!(s, Step::Bar))
                        .map(|i| at + i + 1)
                        .unwrap_or(steps.len());
                    continue;
                }
                // every other token takes the next note
                let next = steps[at..].iter().enumerate().find_map(|(i, s)| match s {
                    Step::Note(note) => Some((i, *note)),
                    Step::Bar => None,
                });
                let Some((i, note)) = next else {
                    left_over = Some(left_over.map_or(token.span, |s| s.join(token.span)));
                    continue;
                };
                at += i + 1;
                let sung = match &token.kind {
                    Lyric::Syllable { text, hyphen } if !text.is_empty() => Sung::Text {
                        text: text.clone(),
                        hyphen: *hyphen,
                    },
                    Lyric::Syllable { .. } => {
                        // a hyphen on its own: the word before goes on past this note
                        if let Some((n, k)) = last_text
                            && let Some(Sung::Text { hyphen, .. }) =
                                underlay.get_mut(&n).and_then(|v| v.get_mut(k)).map(|s| &mut s.sung)
                        {
                            *hyphen = true;
                        }
                        continue;
                    }
                    Lyric::Hold => Sung::Hold,
                    Lyric::Skip | Lyric::Bar => continue,
                };
                let under = underlay.entry(note).or_default();
                if matches!(sung, Sung::Text { .. }) {
                    last_text = Some((note, under.len()));
                }
                under.push(Syllable {
                    verse,
                    sung,
                    span: token.span,
                });
            }
            if let Some(span) = left_over {
                diagnostics.push(Diagnostic::new(span, "there are no notes left for these lyrics"));
            }
        }
    }
    underlay
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voices::voices;

    /// The lyrics of a one-voice tune as `note:syllable`, verse by verse, and
    /// how many diagnostics there were. A held note shows as `note:_`.
    fn sung(body: &str) -> (Vec<String>, usize) {
        let src = format!("X:1\nT:T\nK:C\n{}", body);
        let doc = abc::parse(&src);
        let v = voices(&doc.tunes[0]);
        let mut diagnostics = vec![];
        let underlay = align(&v[0].lyrics, &mut diagnostics);
        let mut notes: Vec<&usize> = underlay.keys().collect();
        notes.sort();
        let verses = underlay.values().flatten().map(|s| s.verse + 1).max().unwrap_or(0);
        let rows = (0..verses)
            .map(|verse| {
                let mut row = vec![];
                for n in &notes {
                    for s in underlay[*n].iter().filter(|s| s.verse == verse) {
                        // the note as written, up to the next space or bar line
                        let note: String = src[**n..].chars().take_while(|c| !" |\n\\".contains(*c)).collect();
                        row.push(match &s.sung {
                            Sung::Text { text, hyphen } => format!("{}:{}{}", note, text, if *hyphen { "-" } else { "" }),
                            Sung::Hold => format!("{}:_", note),
                        });
                    }
                }
                row.join(" ")
            })
            .collect();
        (rows, diagnostics.len())
    }

    fn rows(body: &str) -> Vec<String> {
        let (rows, diagnostics) = sung(body);
        assert_eq!(diagnostics, 0);
        rows
    }

    #[test]
    fn a_syllable_for_each_note() {
        assert_eq!(rows("C D E F|\nw:do re mi fa\n"), vec!["C:do D:re E:mi F:fa"]);
        // fewer syllables than notes is fine
        assert_eq!(rows("C D E F|\nw:do re\n"), vec!["C:do D:re"]);
        // hyphens stay with their syllable
        assert_eq!(rows("C D E F|\nw:syl-la-ble end\n"), vec!["C:syl- D:la- E:ble F:end"]);
        assert_eq!(rows("C D\nw:of~the well\\-known\n"), vec!["C:of the D:well-known"]);
    }

    #[test]
    fn rests_and_grace_notes_take_no_syllable() {
        assert_eq!(rows("C z D {ef}E x F|\nw:a b c d\n"), vec!["C:a D:b E:c F:d"]);
        // a chord is one note, tied notes are two
        assert_eq!(rows("[CEG] D- D\nw:a b c\n"), vec!["[CEG]:a D-:b D:c"]);
        // the music after an & has no syllables
        assert_eq!(rows("C D & E F|G A & B c|\nw:a b c d\n"), vec!["C:a D:b G:c A:d"]);
    }

    #[test]
    fn holds_and_skips() {
        assert_eq!(rows("C D E F|\nw:time__ flies\n"), vec!["C:time D:_ E:_ F:flies"]);
        assert_eq!(rows("C D E F|\nw:a * * b\n"), vec!["C:a F:b"]);
        // a hyphen on its own leaves a note bare inside a word
        assert_eq!(rows("C D E F|\nw:go--ing on\n"), vec!["C:go- E:ing F:on"]);
        assert_eq!(rows("C D E F|\nw:go - ing on\n"), vec!["C:go- E:ing F:on"]);
    }

    #[test]
    fn a_bar_goes_on_after_the_next_bar_line() {
        // short bars are skipped to their end, full bars are not touched
        assert_eq!(rows("C D E|F G A|\nw:a | b c\n"), vec!["C:a F:b G:c"]);
        assert_eq!(rows("C D|E F|\nw:a b | c d |\n"), vec!["C:a D:b E:c F:d"]);
        assert_eq!(rows("C D|E F|G\nw:| | a\n"), vec!["G:a"]);
    }

    #[test]
    fn verses_and_blocks() {
        // lines right after each other are verses
        assert_eq!(rows("C D E|\nw:a b c\nw:x y z\n"), vec!["C:a D:b E:c", "C:x D:y E:z"]);
        // each block belongs to the music since the block before
        assert_eq!(rows("C D E|\nw:a b\nF G A|\nw:c d e\n"), vec!["C:a D:b F:c G:d A:e"]);
        // one block may follow several lines of music
        assert_eq!(rows("C D E|\nF G A|\nw:a b c d e f\n"), vec!["C:a D:b E:c F:d G:e A:f"]);
        // a continued w: line is still one verse
        assert_eq!(rows("C D E F|\nw:a b\n+:c d\n"), vec!["C:a D:b E:c F:d"]);
        // lyrics inside a continued music line
        assert_eq!(rows("C D\\\nw:a b\nE F|\nw:c d\n"), vec!["C:a D:b E:c F:d"]);
    }

    #[test]
    fn too_many_syllables_are_reported() {
        assert_eq!(sung("C D|\nw:a b c d\n"), (vec!["C:a D:b".to_string()], 1));
        assert_eq!(sung("C D|\nw:a | b | c\n"), (vec!["C:a".to_string()], 1));
        // lyrics before any music
        let src = "X:1\nT:T\nK:C\nw:la la\nC D|\n";
        let doc = abc::parse(src);
        let v = voices(&doc.tunes[0]);
        let mut diagnostics = vec![];
        assert!(align(&v[0].lyrics, &mut diagnostics).is_empty());
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(&src[diagnostics[0].span.start..diagnostics[0].span.end], "la la");
    }

    #[test]
    fn each_voice_has_its_own_lyrics() {
        let src = "X:1\nT:T\nK:C\n[V:1]C D|\nw:a b\n[V:2]E F|\nw:c d\n";
        let doc = abc::parse(src);
        let v = voices(&doc.tunes[0]);
        let mut diagnostics = vec![];
        let first = align(&v[0].lyrics, &mut diagnostics);
        let second = align(&v[1].lyrics, &mut diagnostics);
        assert!(diagnostics.is_empty());
        assert_eq!(first.len(), 2);
        assert!(first.contains_key(&src.find("C D").unwrap()));
        assert_eq!(second.len(), 2);
        assert!(second.contains_key(&src.find("E F").unwrap()));
    }
}
