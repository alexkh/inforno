  //============================================================================
 // abc/src/datatypes/mod.rs
//==============================================================================

//! Structures representing parsed ABC music.
//!
//! The source text is the document. Every parsed item carries a [`Span`] into
//! that text, and values are kept as written (lengths are multiples of `L:`,
//! broken rhythm and tuplets are markers), so an edit can replace exactly the
//! text of the item that changed and leave the rest of the file untouched.

use num_rational::Rational32;

pub mod writer;

//------------------------------------------------------------------------------
// spans and diagnostics

/// A byte range `start..end` into the source text.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Span {
        Span { start, end }
    }

    pub fn len(&self) -> usize {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// The same range moved `by` bytes to the right.
    pub fn offset(self, by: usize) -> Span {
        Span::new(self.start + by, self.end + by)
    }

    pub fn contains(&self, pos: usize) -> bool {
        self.start <= pos && pos < self.end
    }

    /// The smallest span covering both `self` and `other`.
    pub fn join(self, other: Span) -> Span {
        Span::new(self.start.min(other.start), self.end.max(other.end))
    }
}

/// Something in the source that could not be understood.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub span: Span,
    pub message: String,
}

impl Diagnostic {
    pub fn new(span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic { span, message: message.into() }
    }
}

//------------------------------------------------------------------------------
// document structure

/// A whole ABC file: an optional file header followed by tunes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Document {
    /// Fields before the first tune; they act as defaults for every tune.
    pub file_header: Vec<InfoField>,
    /// `%%` directives before the first tune.
    pub file_directives: Vec<Directive>,
    pub tunes: Vec<Tune>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Document {
    /// The tune whose text contains byte `pos`, if any.
    pub fn tune_at(&self, pos: usize) -> Option<&Tune> {
        self.tunes.iter().find(|t| t.span.contains(pos))
    }
}

/// One tune, from its `X:` line to the blank line (or end of file) after it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tune {
    pub span: Span,
    /// `X:` up to and including `K:`.
    pub header: Vec<InfoField>,
    /// `%%` directives written in the header, such as `%%score`.
    pub directives: Vec<Directive>,
    pub body: Vec<BodyLine>,
}

impl Tune {
    /// The value of the first header field called `name`.
    pub fn field(&self, name: char) -> Option<&str> {
        self.header.iter().find(|f| f.name == name).map(|f| f.value.as_str())
    }

    /// The value of the last header directive called `name` (`"score"` for `%%score`).
    pub fn directive(&self, name: &str) -> Option<&str> {
        self.directives.iter().rev().find(|d| d.name == name).map(|d| d.value.as_str())
    }

    /// All music lines of the body, in order.
    pub fn music_lines(&self) -> impl Iterator<Item = &MusicLine> {
        self.body.iter().filter_map(|l| match l {
            BodyLine::Music(m) => Some(m),
            _ => None,
        })
    }
}

/// A line of a tune body.
#[derive(Clone, Debug, PartialEq)]
pub enum BodyLine {
    Music(MusicLine),
    /// A field line inside the body, such as `K:`, `M:`, `L:`, `P:` or `V:`.
    Field(InfoField),
    /// A `w:` line: lyrics for the notes above it.
    Lyrics(LyricLine),
    /// A `%` comment line.
    Comment(Span),
    /// A `%%` directive line.
    Directive(Directive),
}

/// A field such as `T:Title`. `value` is trimmed and has comments removed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InfoField {
    pub name: char,
    pub value: String,
    pub span: Span,
}

impl InfoField {
    pub fn new(name: char, value: impl Into<String>, span: Span) -> InfoField {
        InfoField { name, value: value.into(), span }
    }
}

/// A directive line such as `%%score (1 2)`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Directive {
    /// The whole line.
    pub span: Span,
    /// The word after `%%`: `score`.
    pub name: String,
    /// The rest of the line as written, trimmed: `(1 2)`.
    pub value: String,
}

/// A `w:` line, together with any `+:` lines that continue it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LyricLine {
    pub span: Span,
    pub tokens: Vec<LyricToken>,
}

/// One item of a lyric line together with the text it was parsed from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LyricToken {
    pub span: Span,
    pub kind: Lyric,
}

impl LyricToken {
    pub fn new(span: Span, kind: Lyric) -> LyricToken {
        LyricToken { span, kind }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Lyric {
    /// Text sung on one note. `~` is already a space and `\-` a hyphen.
    /// `hyphen` is true when a `-` follows: the word goes on with the next
    /// syllable. A `-` after a space or another `-` is a syllable with no
    /// text: it takes a note and the word goes on after it.
    Syllable { text: String, hyphen: bool },
    /// `_`: the previous syllable is held for one more note.
    Hold,
    /// `*`: one note is skipped.
    Skip,
    /// `|`: go on at the next bar.
    Bar,
}

/// One line of music.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MusicLine {
    /// The whole line, without the line terminator.
    pub span: Span,
    pub symbols: Vec<Symbol>,
    /// The line ended with `\`, so the next music line continues the same staff line.
    pub continues: bool,
}

/// A symbol together with the text it was parsed from.
#[derive(Clone, Debug, PartialEq)]
pub struct Symbol {
    pub span: Span,
    pub kind: MusicSymbol,
}

impl Symbol {
    pub fn new(span: Span, kind: MusicSymbol) -> Symbol {
        Symbol { span, kind }
    }
}

//------------------------------------------------------------------------------
// music symbols

#[derive(Clone, Debug, PartialEq)]
pub struct Pitch {
    pub note: Note,
    pub octave: i8,
    pub accidental: Option<Accidental>,
    pub tie: Option<Tie>,
    /// Sharp notation: the semitone above `note`, written `I K N Q W`
    /// (C# D# F# G# A#) and drawn with its own notehead on `note`'s line or space.
    pub raised: bool,
}

impl Pitch {
    pub fn new(note: Note, octave: i8, accidental: Option<Accidental>, tie: Option<Tie>) -> Self {
        Pitch {
            note,
            octave,
            accidental,
            tie,
            raised: false,
        }
    }

    /// A sharp-notation pitch: the semitone above `note` (C, D, F, G or A).
    pub fn raised(note: Note, octave: i8) -> Self {
        Pitch {
            raised: true,
            ..Pitch::new(note, octave, None, None)
        }
    }

    /// Diatonic steps above middle C (C4 = 0, D4 = 1, C5 = 7, B3 = -1).
    pub fn diatonic(&self) -> i32 {
        (self.octave as i32 - 1) * 7 + self.note.index()
    }
}

/// Where an annotation (`"^text"` and friends) is placed relative to its note.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Placement {
    Above,
    Below,
    Left,
    Right,
    Free,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MusicSymbol {
    /// A note or chord. `length` is the written multiple of `L:`.
    Stem {
        pitches: Vec<Pitch>,
        length: Rational32,
        decorations: Vec<Decoration>,
        tie: Option<Tie>,
    },
    Rest(Rest),
    Bar(String),
    Ending(i32),
    /// One or more spaces; notes separated by a space are not beamed together.
    VisualBreak,
    SlurBegin,
    SlurEnd,
    /// `(p:q:r`; it applies to the next `r` notes (default `p`).
    TupletStart {
        p: i32,
        q: Option<i32>,
        r: Option<i32>,
    },
    /// `{` or `{/`
    GraceStart {
        acciaccatura: bool,
    },
    /// `}`
    GraceEnd,
    /// `>`, `>>`, `<` ... between two notes. `first_longer` is true for `>`.
    BrokenRhythm {
        first_longer: bool,
        dots: u8,
    },
    /// `"Am"`
    ChordSymbol(String),
    /// `"^text"`, `"_text"`, `"<text"`, `">text"`, `"@text"`
    Annotation {
        placement: Placement,
        text: String,
    },
    /// `[K:G]`, `[M:3/4]` ...
    InlineField(InfoField),
    /// Decorations not directly followed by a note, such as `!fermata!z` or `!segno!|`.
    Decorations(Vec<Decoration>),
    /// `&`: the music from here to the next bar line sounds together with
    /// what the voice has played since the bar line before.
    Overlay,
    /// Text that could not be parsed; parsing resumes at the next space or bar line.
    Unknown(String),
}

/// A tie which may apply between a note and the following note of the same pitch.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Tie {
    /// A normal tie.
    Solid,
    /// A dotted tie.
    Dotted,
}

/// The default `q` of a tuplet `(p` when the tune does not give one.
/// `compound` is true for compound meters such as 6/8, 9/8 or 12/8.
pub fn default_tuplet_q(p: i32, compound: bool) -> i32 {
    match p {
        2 | 4 | 8 => 3,
        3 | 6 => 2,
        _ => {
            if compound {
                3
            } else {
                2
            }
        }
    }
}

impl MusicSymbol {
    // Constructor helper for single notes (Stems with 1 pitch)
    pub fn new_note(
        decorations: Vec<Decoration>,
        accidental: Option<Accidental>,
        note: Note,
        octave: i8,
        length: impl Into<Rational32>,
        tie: Option<Tie>,
    ) -> MusicSymbol {
        MusicSymbol::Stem {
            pitches: vec![Pitch::new(note, octave, accidental, None)],
            length: length.into(),
            decorations,
            tie, // single notes keep their tie on the Stem, after the length ("c4-")
        }
    }

    // Helper for tests
    pub fn note(note: Note) -> MusicSymbol {
        Self::new_note(vec![], None, note, 1, 1, None)
    }

    pub fn note_from_length(note: Note, length: impl Into<Rational32>) -> MusicSymbol {
        Self::new_note(vec![], None, note, 1, length, None)
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Note {
    C,
    D,
    E,
    F,
    G,
    A,
    B,
}

impl Note {
    /// C = 0 ... B = 6
    pub fn index(self) -> i32 {
        match self {
            Note::C => 0,
            Note::D => 1,
            Note::E => 2,
            Note::F => 3,
            Note::G => 4,
            Note::A => 5,
            Note::B => 6,
        }
    }
}

impl From<Note> for char {
    fn from(note: Note) -> Self {
        match note {
            Note::C => 'C',
            Note::D => 'D',
            Note::E => 'E',
            Note::F => 'F',
            Note::G => 'G',
            Note::A => 'A',
            Note::B => 'B',
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Decoration {
    Staccato,
    Roll,
    Fermata,
    Accent,
    LowerMordent,
    Coda,
    UpperMordent,
    Segno,
    Trill,
    UpBow,
    DownBow,
    /// `!name!`, kept as written; the engraver decides what it means.
    Unresolved(String),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Accidental {
    Natural,
    Sharp,
    Flat,
    DoubleSharp,
    DoubleFlat,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Rest {
    Note(Rational32),
    Measure(i32),
    NoteHidden(Rational32),
    MeasureHidden(i32),
}
