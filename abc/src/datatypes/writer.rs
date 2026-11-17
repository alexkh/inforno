  //============================================================================
 // abc/src/datatypes/writer.rs
//==============================================================================

//! Turns datatype representations of ABC back into text.
//!
//! The source text stays the document, so this is only needed to produce the
//! text of a symbol that was created or changed by an edit.

use super::*;

pub trait ToABC {
    /// Creates a valid ABC text representation of the object.
    fn to_abc(&self) -> String;
}

impl<T> ToABC for Option<T>
where
    T: ToABC,
{
    fn to_abc(&self) -> String {
        match self {
            Some(h) => h.to_abc(),
            None => String::new(),
        }
    }
}

impl ToABC for InfoField {
    fn to_abc(&self) -> String {
        format!("{}:{}", self.name, self.value)
    }
}

impl ToABC for Directive {
    fn to_abc(&self) -> String {
        if self.value.is_empty() {
            format!("%%{}", self.name)
        } else {
            format!("%%{} {}", self.name, self.value)
        }
    }
}

impl ToABC for LyricLine {
    /// One space between tokens, none after a hyphen and none before `_`.
    fn to_abc(&self) -> String {
        let mut s = String::from("w:");
        let mut joined = true;
        for t in &self.tokens {
            if !joined && t.kind != Lyric::Hold {
                s.push(' ');
            }
            joined = false;
            match &t.kind {
                Lyric::Syllable { text, hyphen } => {
                    for c in text.chars() {
                        match c {
                            ' ' => s.push('~'),
                            '-' => s.push_str("\\-"),
                            c => s.push(c),
                        }
                    }
                    if *hyphen {
                        s.push('-');
                        joined = true;
                    }
                }
                Lyric::Hold => s.push('_'),
                Lyric::Skip => s.push('*'),
                Lyric::Bar => s.push('|'),
            }
        }
        s
    }
}

impl ToABC for Symbol {
    fn to_abc(&self) -> String {
        self.kind.to_abc()
    }
}

impl ToABC for MusicLine {
    fn to_abc(&self) -> String {
        let mut s: String = self.symbols.iter().map(ToABC::to_abc).collect();
        if self.continues {
            s.push('\\');
        }
        s
    }
}

fn format_pitch(p: &Pitch) -> String {
    let (note, octave) = denormalise_octave(p.note, p.octave);
    let note = if p.raised { raised_letter(note) } else { note };
    format!("{}{}{}", p.accidental.to_abc(), note, octave_to_abc(octave))
}

/// Sharp notation: the letter for the semitone above `note` (C -> I, D -> K, F -> N, G -> Q, A -> W).
fn raised_letter(note: char) -> char {
    let raised = match note.to_ascii_uppercase() {
        'C' => 'I',
        'D' => 'K',
        'F' => 'N',
        'G' => 'Q',
        'A' => 'W',
        other => other, // E and B have no semitone above in sharp notation
    };
    if note.is_ascii_lowercase() {
        raised.to_ascii_lowercase()
    } else {
        raised
    }
}

impl ToABC for MusicSymbol {
    fn to_abc(&self) -> String {
        use super::MusicSymbol::*;
        match self {
            VisualBreak => String::from(" "),
            SlurBegin => String::from("("),
            SlurEnd => String::from(")"),
            Stem {
                pitches,
                length,
                decorations,
                tie,
            } => {
                let core = if pitches.len() == 1 {
                    // a single note keeps its tie on the Stem, after the length
                    format_pitch(&pitches[0])
                } else {
                    let mut s = String::from("[");
                    for p in pitches {
                        s.push_str(&format_pitch(p));
                        s.push_str(&p.tie.to_abc());
                    }
                    s.push(']');
                    s
                };
                format!(
                    "{}{}{}{}",
                    decorations.to_abc(),
                    core,
                    length_to_abc(*length),
                    tie.to_abc()
                )
            }
            Bar(bar) => bar.clone(),
            Rest(rest) => rest.to_abc(),
            Ending(n) => format!("[{}", n),
            TupletStart { p, q, r } => match (q, r) {
                (None, None) => format!("({}", p),
                (Some(q), None) => format!("({}:{}", p, q),
                (q, Some(r)) => format!("({}:{}:{}", p, q.map(|q| q.to_string()).unwrap_or_default(), r),
            },
            GraceStart { acciaccatura } => {
                if *acciaccatura {
                    String::from("{/")
                } else {
                    String::from("{")
                }
            }
            GraceEnd => String::from("}"),
            BrokenRhythm { first_longer, dots } => {
                let c = if *first_longer { ">" } else { "<" };
                c.repeat(*dots as usize)
            }
            ChordSymbol(text) => format!("\"{}\"", text),
            Annotation { placement, text } => {
                let c = match placement {
                    Placement::Above => '^',
                    Placement::Below => '_',
                    Placement::Left => '<',
                    Placement::Right => '>',
                    Placement::Free => '@',
                };
                format!("\"{}{}\"", c, text)
            }
            InlineField(f) => format!("[{}]", f.to_abc()),
            Decorations(d) => d.to_abc(),
            Overlay => String::from("&"),
            Unknown(text) => text.clone(),
        }
    }
}

impl ToABC for Tie {
    fn to_abc(&self) -> String {
        match self {
            Tie::Solid => "-",
            Tie::Dotted => ".-",
        }
        .to_string()
    }
}

/// Given a note and an octave, returns the uppercase or lowercase form of the note which makes the
/// octave closest to 1, and the octave adjusted accordingly.
fn denormalise_octave(note: Note, octave: i8) -> (char, i8) {
    let note_char: char = note.into();
    if octave > 1 {
        (note_char.to_ascii_lowercase(), octave - 1)
    } else {
        (note_char, octave)
    }
}

impl ToABC for Decoration {
    fn to_abc(&self) -> String {
        use super::Decoration::*;
        match self {
            Staccato => String::from("."),
            Roll => String::from("~"),
            Fermata => String::from("H"),
            Accent => String::from("L"),
            LowerMordent => String::from("M"),
            Coda => String::from("O"),
            UpperMordent => String::from("P"),
            Segno => String::from("S"),
            Trill => String::from("T"),
            UpBow => String::from("u"),
            DownBow => String::from("v"),
            Unresolved(s) => format!("!{}!", s),
        }
    }
}

impl ToABC for Vec<Decoration> {
    fn to_abc(&self) -> String {
        self.iter().map(ToABC::to_abc).collect()
    }
}

impl ToABC for Accidental {
    fn to_abc(&self) -> String {
        use super::Accidental::*;
        String::from(match self {
            Natural => "=",
            Sharp => "^",
            Flat => "_",
            DoubleSharp => "^^",
            DoubleFlat => "__",
        })
    }
}

fn octave_to_abc(octave: i8) -> String {
    match octave {
        1 => String::new(),
        o if o > 1 => "'".repeat((o - 1) as usize),
        o => ",".repeat((1 - o) as usize),
    }
}

fn length_to_abc(length: Rational32) -> String {
    if length.is_integer() {
        if length.to_integer() == 1 {
            String::new()
        } else {
            length.to_integer().to_string()
        }
    } else if *length.numer() == 1 && *length.denom() == 2 {
        // "1/2" -> "/"
        "/".to_string()
    } else if *length.numer() == 1 {
        format!("/{}", length.denom())
    } else {
        format!("{}/{}", length.numer(), length.denom())
    }
}

impl ToABC for Rest {
    fn to_abc(&self) -> String {
        use super::Rest::*;

        match self {
            Note(n) => format!("z{}", length_to_abc(*n)),
            NoteHidden(n) => format!("x{}", length_to_abc(*n)),
            Measure(n) => if *n == 1 { "Z".to_string() } else { format!("Z{}", n) },
            MeasureHidden(n) => if *n == 1 { "X".to_string() } else { format!("X{}", n) },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_field() {
        let f = InfoField::new('X', "1", Span::default());
        assert_eq!(f.to_abc(), "X:1")
    }

    #[test]
    fn directive() {
        let d = crate::parse("X:1\n%%score  (1 2)\n%%vskip\nK:C\n");
        assert_eq!(d.tunes[0].directives[0].to_abc(), "%%score (1 2)");
        assert_eq!(d.tunes[0].directives[1].to_abc(), "%%vskip");
    }

    #[test]
    fn lyric_line() {
        let written = |w: &str| {
            let d = crate::parse(&format!("X:1\nK:C\nabc\n{}\n", w));
            match &d.tunes[0].body[1] {
                BodyLine::Lyrics(l) => l.to_abc(),
                l => panic!("expected lyrics, got {:?}", l),
            }
        };
        assert_eq!(written("w: syll-a-ble  time__ * |  of~the \\-end"), "w:syll-a-ble time__ * | of~the \\-end");
        assert_eq!(written("w:go--ing a - b"), "w:go--ing a -b");
        assert_eq!(written("w:syll-_able time _"), "w:syll-_ able time_");
        // what is written reads back as the same tokens
        assert_eq!(written("w:go--ing a -b"), "w:go--ing a -b");
    }

    #[test]
    fn accidental_1() {
        assert_eq!(Accidental::Natural.to_abc(), "=")
    }
    #[test]
    fn accidental_2() {
        assert_eq!(Accidental::Sharp.to_abc(), "^")
    }
    #[test]
    fn accidental_3() {
        assert_eq!(Accidental::Flat.to_abc(), "_")
    }
    #[test]
    fn accidental_4() {
        assert_eq!(Accidental::DoubleSharp.to_abc(), "^^")
    }
    #[test]
    fn accidental_5() {
        assert_eq!(Accidental::DoubleFlat.to_abc(), "__")
    }

    #[test]
    fn note_with_accidental() {
        assert_eq!(
            MusicSymbol::new_note(vec![], Some(Accidental::Sharp), Note::C, 1,
            Rational32::from_integer(1), None).to_abc(),
            "^C"
        );
    }

    #[test]
    fn octaves() {
        let n = |o| MusicSymbol::new_note(vec![], None, Note::C, o, 1, None).to_abc();
        assert_eq!(n(1), "C");
        assert_eq!(n(0), "C,");
        assert_eq!(n(-1), "C,,");
        assert_eq!(n(2), "c");
        assert_eq!(n(3), "c'");
        assert_eq!(n(4), "c''");
    }

    #[test]
    fn decoration_1() {
        assert_eq!(Decoration::Staccato.to_abc(), ".")
    }
    #[test]
    fn decoration_2() {
        assert_eq!(Decoration::Roll.to_abc(), "~")
    }
    #[test]
    fn decoration_3() {
        assert_eq!(Decoration::Fermata.to_abc(), "H")
    }
    #[test]
    fn decoration_4() {
        assert_eq!(Decoration::Accent.to_abc(), "L")
    }
    #[test]
    fn decoration_5() {
        assert_eq!(Decoration::LowerMordent.to_abc(), "M")
    }
    #[test]
    fn decoration_6() {
        assert_eq!(Decoration::Coda.to_abc(), "O")
    }
    #[test]
    fn decoration_7() {
        assert_eq!(Decoration::UpperMordent.to_abc(), "P")
    }
    #[test]
    fn decoration_8() {
        assert_eq!(Decoration::Segno.to_abc(), "S")
    }
    #[test]
    fn decoration_9() {
        assert_eq!(Decoration::Trill.to_abc(), "T")
    }
    #[test]
    fn decoration_10() {
        assert_eq!(Decoration::UpBow.to_abc(), "u")
    }
    #[test]
    fn decoration_11() {
        assert_eq!(Decoration::DownBow.to_abc(), "v")
    }
    #[test]
    fn decoration_12() {
        assert_eq!(
            Decoration::Unresolved("asdf".to_string()).to_abc(),
            "!asdf!"
        )
    }

    #[test]
    fn rest_1() {
        assert_eq!(Rest::Note(Rational32::from_integer(1)).to_abc(), "z")
    }
    #[test]
    fn rest_2() {
        assert_eq!(Rest::Note(Rational32::from_integer(2)).to_abc(), "z2")
    }
    #[test]
    fn rest_3() {
        assert_eq!(Rest::Measure(1).to_abc(), "Z")
    }
    #[test]
    fn rest_4() {
        assert_eq!(Rest::Measure(2).to_abc(), "Z2")
    }
    #[test]
    fn rest_5() {
        assert_eq!(Rest::NoteHidden(Rational32::from_integer(1)).to_abc(), "x")
    }
    #[test]
    fn rest_6() {
        assert_eq!(Rest::NoteHidden(Rational32::from_integer(4)).to_abc(), "x4")
    }
    #[test]
    fn rest_7() {
        assert_eq!(Rest::MeasureHidden(1).to_abc(), "X")
    }
    #[test]
    fn rest_8() {
        assert_eq!(Rest::MeasureHidden(3).to_abc(), "X3")
    }
    #[test]
    fn rest_9() {
        assert_eq!(Rest::Note(Rational32::new(1, 4)).to_abc(), "z/4")
    }

    #[test]
    fn ending_1() {
        assert_eq!(MusicSymbol::Ending(1).to_abc(), "[1")
    }
    #[test]
    fn ending_2() {
        assert_eq!(MusicSymbol::Ending(2).to_abc(), "[2")
    }

    #[test]
    fn grace_notes() {
        assert_eq!(MusicSymbol::GraceStart { acciaccatura: false }.to_abc(), "{");
        assert_eq!(MusicSymbol::GraceStart { acciaccatura: true }.to_abc(), "{/");
        assert_eq!(MusicSymbol::GraceEnd.to_abc(), "}");
        assert_eq!(MusicSymbol::Overlay.to_abc(), "&");
    }

    #[test]
    fn tuplets() {
        let t = |p, q, r| MusicSymbol::TupletStart { p, q, r }.to_abc();
        assert_eq!(t(3, None, None), "(3");
        assert_eq!(t(3, Some(2), None), "(3:2");
        assert_eq!(t(3, Some(2), Some(3)), "(3:2:3");
        assert_eq!(t(3, None, Some(2)), "(3::2");
    }

    #[test]
    fn quoted() {
        assert_eq!(MusicSymbol::ChordSymbol("Am".into()).to_abc(), "\"Am\"");
        assert_eq!(
            MusicSymbol::Annotation { placement: Placement::Above, text: "I".into() }.to_abc(),
            "\"^I\""
        );
    }

    #[test]
    fn tie() {
        assert_eq!(
            MusicSymbol::new_note(vec![], None, Note::C, 1,
            Rational32::from_integer(1), Some(Tie::Solid)).to_abc(),
            "C-"
        );
    }

    #[test]
    fn raised_notes() {
        let n = |note, octave| MusicSymbol::Stem {
            pitches: vec![Pitch::raised(note, octave)],
            length: Rational32::from_integer(1),
            decorations: vec![],
            tie: None,
        }
        .to_abc();
        assert_eq!(n(Note::C, 1), "I");
        assert_eq!(n(Note::D, 1), "K");
        assert_eq!(n(Note::F, 2), "n");
        assert_eq!(n(Note::G, 3), "q'");
        assert_eq!(n(Note::A, 0), "W,");
    }

    #[test]
    fn chord_with_inner_ties() {
        let c = MusicSymbol::Stem {
            pitches: vec![
                Pitch::new(Note::C, 1, None, Some(Tie::Solid)),
                Pitch::new(Note::E, 1, None, None),
            ],
            length: Rational32::from_integer(2),
            decorations: vec![],
            tie: None,
        };
        assert_eq!(c.to_abc(), "[C-E]2");
    }
}
