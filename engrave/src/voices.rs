//! Voices: what `V:` fields say, and which music belongs to which voice.
//!
//! The header declares voices with `V:` fields. In the body a `V:` line or an
//! inline `[V:]` field says which voice the music after it belongs to. A tune
//! without any `V:` has one voice.
//!
//! A `K:`, `M:` or `L:` line of the body applies to every voice until the
//! body names a voice, and after that to the voice it is written in.
//!
//! A `w:` line belongs to the voice it is written in, and is sung to the
//! music of that voice since its previous `w:` lines.

use crate::music::{Clef, clef_name};
use abc::datatypes::*;

/// The voice of music that no `V:` field claims.
const DEFAULT_VOICE: &str = "1";

/// What `V:` fields say about a voice.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VoiceProps {
    /// `name=` or `nm=`: printed before the voice's first staff.
    pub name: Option<String>,
    /// `subname=` or `snm=`: printed before its later staves.
    pub subname: Option<String>,
    pub clef: Option<Clef>,
    /// `middle=`: the written pitch that sits on the middle line of the
    /// staff, as a diatonic number (C4 = 0).
    pub middle: Option<i32>,
    /// `octave=`: how many octaves higher the notes are drawn.
    pub octave: Option<i32>,
    /// `stem=up` is `Some(true)`, `stem=down` is `Some(false)`.
    pub stem: Option<bool>,
}

impl VoiceProps {
    /// Takes over everything that `other` says.
    fn merge(&mut self, other: VoiceProps) {
        self.name = other.name.or(self.name.take());
        self.subname = other.subname.or(self.subname.take());
        self.clef = other.clef.or(self.clef);
        self.middle = other.middle.or(self.middle);
        self.octave = other.octave.or(self.octave);
        self.stem = other.stem.or(self.stem);
    }
}

/// The words of a field value. Double quotes keep spaces inside a word and
/// are left out: `name="Tenore I"` is the one word `name=Tenore I`.
fn words(value: &str) -> Vec<String> {
    let mut words = vec![];
    let mut word = String::new();
    let mut started = false;
    let mut quoted = false;
    for c in value.chars() {
        if c == '"' {
            quoted = !quoted;
            started = true;
        } else if c.is_whitespace() && !quoted {
            if started {
                words.push(std::mem::take(&mut word));
                started = false;
            }
        } else {
            word.push(c);
            started = true;
        }
    }
    if started {
        words.push(word);
    }
    words
}

/// The diatonic number (C4 = 0) of a pitch written as in the music: `d`, `B,`.
fn pitch_position(s: &str) -> Option<i32> {
    let mut chars = s.chars();
    let letter = chars.next()?;
    let step = match letter.to_ascii_uppercase() {
        'C' => 0,
        'D' => 1,
        'E' => 2,
        'F' => 3,
        'G' => 4,
        'A' => 5,
        'B' => 6,
        _ => return None,
    };
    let mut d = if letter.is_ascii_lowercase() { step + 7 } else { step };
    for c in chars {
        match c {
            '\'' => d += 7,
            ',' => d -= 7,
            _ => return None,
        }
    }
    // nothing sensible lies further than this from the staff
    (-40..=40).contains(&d).then_some(d)
}

/// Reads the value of a `V:` field: the id of the voice and what the field
/// says about it. Properties this engraver has no use for, such as
/// `transpose=`, are skipped.
pub fn parse_voice(value: &str) -> (String, VoiceProps) {
    let mut props = VoiceProps::default();
    let mut id = None;
    for (i, word) in words(value).into_iter().enumerate() {
        let Some((key, v)) = word.split_once('=') else {
            if i == 0 {
                id = Some(word);
            } else if word.len() > 1 {
                // a clef may be given without "clef="
                props.clef = clef_name(&word.to_ascii_lowercase()).or(props.clef);
            }
            continue;
        };
        match key.to_ascii_lowercase().as_str() {
            "name" | "nm" => props.name = Some(v.to_string()),
            "subname" | "sname" | "snm" => props.subname = Some(v.to_string()),
            "clef" => props.clef = clef_name(&v.to_ascii_lowercase()).or(props.clef),
            "middle" | "m" => props.middle = pitch_position(v).or(props.middle),
            "octave" => props.octave = v.parse::<i32>().ok().map(|o| o.clamp(-4, 4)).or(props.octave),
            "stem" | "stems" => match v.to_ascii_lowercase().as_str() {
                "up" => props.stem = Some(true),
                "down" => props.stem = Some(false),
                _ => {}
            },
            _ => {}
        }
    }
    (id.unwrap_or_else(|| DEFAULT_VOICE.to_string()), props)
}

/// What happens in a voice, in the order it is written.
#[derive(Debug)]
pub enum Event<'a> {
    /// One line of music: a music line, joined with the lines that continue
    /// it, or the part of one that an inline `[V:]` gives to this voice.
    Line(Vec<&'a Symbol>),
    /// A `K:`, `M:` or `L:` line of the body.
    Field(&'a InfoField),
}

/// `w:` lines that follow each other, and the music they are sung to.
#[derive(Debug)]
pub struct LyricBlock<'a> {
    /// One line for each verse.
    pub verses: Vec<&'a LyricLine>,
    /// The music of the voice since its lyrics before.
    pub music: Vec<&'a Symbol>,
}

#[derive(Debug)]
pub struct Voice<'a> {
    pub id: String,
    pub props: VoiceProps,
    /// The first `V:` field that names the voice.
    pub span: Option<Span>,
    /// Named by a `V:` field of the header.
    declared: bool,
    pub events: Vec<Event<'a>>,
    pub lyrics: Vec<LyricBlock<'a>>,
    /// The music since the voice's last lyrics.
    since: Vec<&'a Symbol>,
}

impl<'a> Voice<'a> {
    fn new(id: String, declared: bool, events: Vec<Event<'a>>) -> Voice<'a> {
        Voice {
            id,
            props: VoiceProps::default(),
            span: None,
            declared,
            events,
            lyrics: vec![],
            since: vec![],
        }
    }

    /// How many lines of music the voice has.
    pub fn lines(&self) -> usize {
        self.events.iter().filter(|e| matches!(e, Event::Line(_))).count()
    }
}

struct Walk<'a> {
    voices: Vec<Voice<'a>>,
    current: usize,
    /// The body has named a voice.
    named: bool,
    /// Body fields from before the body named a voice.
    common: Vec<&'a InfoField>,
    /// The symbols of the line being read.
    pending: Vec<&'a Symbol>,
}

impl<'a> Walk<'a> {
    /// The index of the voice a `V:` field names; a new voice is added.
    fn voice(&mut self, f: &'a InfoField, declared: bool) -> usize {
        let (id, props) = parse_voice(&f.value);
        let i = match self.voices.iter().position(|v| v.id == id) {
            Some(i) => i,
            None => {
                let events = self.common.iter().copied().map(Event::Field).collect();
                self.voices.push(Voice::new(id, declared, events));
                self.voices.len() - 1
            }
        };
        let voice = &mut self.voices[i];
        voice.props.merge(props);
        voice.span = voice.span.or(Some(f.span));
        i
    }

    /// A `V:` line or an inline `[V:]`: the music goes on in another voice.
    fn switch(&mut self, f: &'a InfoField) {
        if !self.pending.is_empty() {
            // the music so far has a voice, even if no `V:` named it
            self.ensure_current();
        }
        let i = self.voice(f, false);
        if i != self.current {
            self.close();
            self.current = i;
        }
        self.named = true;
    }

    /// Ends the line being read.
    fn close(&mut self) {
        let line = std::mem::take(&mut self.pending);
        if line.iter().any(|s| s.kind != MusicSymbol::VisualBreak) {
            self.ensure_current();
            self.voices[self.current].events.push(Event::Line(line));
        }
    }

    /// A symbol of the line being read.
    fn push(&mut self, sym: &'a Symbol) {
        self.ensure_current();
        self.pending.push(sym);
        self.voices[self.current].since.push(sym);
    }

    /// A `w:` line: lyrics for the music of the current voice since its last
    /// lyrics. A line right after another one is the next verse.
    fn lyrics(&mut self, l: &'a LyricLine) {
        self.ensure_current();
        let voice = &mut self.voices[self.current];
        if voice.since.is_empty()
            && let Some(block) = voice.lyrics.last_mut()
        {
            block.verses.push(l);
        } else {
            let music = std::mem::take(&mut voice.since);
            voice.lyrics.push(LyricBlock { verses: vec![l], music });
        }
    }

    fn field(&mut self, f: &'a InfoField) {
        if self.named {
            self.voices[self.current].events.push(Event::Field(f));
        } else {
            for v in self.voices.iter_mut() {
                v.events.push(Event::Field(f));
            }
            self.common.push(f);
        }
    }

    /// Music that no `V:` claims belongs to the first voice; a tune that
    /// declares none gets one.
    fn ensure_current(&mut self) {
        if self.voices.is_empty() {
            let events = self.common.iter().copied().map(Event::Field).collect();
            self.voices.push(Voice::new(DEFAULT_VOICE.to_string(), false, events));
            self.current = 0;
        }
    }
}

/// The voices of a tune, in the order they first appear, each with its music.
/// Voices of the header are kept even without music; a tune without any music
/// has no voices.
pub fn voices(tune: &Tune) -> Vec<Voice<'_>> {
    let mut w = Walk {
        voices: vec![],
        current: 0,
        named: false,
        common: vec![],
        pending: vec![],
    };
    for f in tune.header.iter().filter(|f| f.name == 'V') {
        w.voice(f, true);
    }
    for line in &tune.body {
        match line {
            BodyLine::Music(m) => {
                for sym in &m.symbols {
                    match &sym.kind {
                        MusicSymbol::InlineField(f) if f.name == 'V' => w.switch(f),
                        _ => w.push(sym),
                    }
                }
                if !m.continues {
                    w.close();
                }
            }
            BodyLine::Field(f) => match f.name {
                'V' => w.switch(f),
                'K' | 'M' => {
                    w.close();
                    w.field(f);
                }
                // a line that is being continued has not been pushed yet, so
                // this lands before it and applies to all of it
                'L' => w.field(f),
                _ => {}
            },
            BodyLine::Lyrics(l) => w.lyrics(l),
            BodyLine::Comment(_) | BodyLine::Directive(_) => {}
        }
    }
    w.close();

    let any_music = w.voices.iter().any(|v| v.lines() > 0);
    w.voices.retain(|v| any_music && (v.declared || v.lines() > 0));
    w.voices
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_fields() {
        let (id, p) = parse_voice(r#"T1 clef=treble-8 name="Tenore I" snm="T.I""#);
        assert_eq!(id, "T1");
        assert_eq!(p.name.as_deref(), Some("Tenore I"));
        assert_eq!(p.subname.as_deref(), Some("T.I"));
        assert_eq!(p.clef, Some(Clef::Treble));
        assert_eq!(p.middle, None);

        let (id, p) = parse_voice(r#"B1 middle=d clef=bass name="Basso I" snm="B.I" transpose=-24"#);
        assert_eq!(id, "B1");
        assert_eq!(p.clef, Some(Clef::Bass));
        assert_eq!(p.middle, Some(8));

        let (id, p) = parse_voice("2 bass stem=down octave=-1 nm=Bass");
        assert_eq!(id, "2");
        assert_eq!(p.clef, Some(Clef::Bass));
        assert_eq!(p.stem, Some(false));
        assert_eq!(p.octave, Some(-1));
        assert_eq!(p.name.as_deref(), Some("Bass"));

        assert_eq!(parse_voice("1"), ("1".to_string(), VoiceProps::default()));
        assert_eq!(parse_voice("").0, "1");
        assert_eq!(parse_voice("clef=bass").0, "1");
    }

    #[test]
    fn pitch_positions() {
        assert_eq!(pitch_position("C"), Some(0));
        assert_eq!(pitch_position("B"), Some(6));
        assert_eq!(pitch_position("d"), Some(8));
        assert_eq!(pitch_position("D,"), Some(-6));
        assert_eq!(pitch_position("c'"), Some(14));
        assert_eq!(pitch_position("x"), None);
        assert_eq!(pitch_position(""), None);
        assert_eq!(pitch_position("c''''''''''"), None);
    }

    /// The voices of the tune's body as `id: line | line`, where a line is its
    /// source text, and fields are shown as `K:G`.
    fn outline(body: &str) -> Vec<String> {
        let src = format!("X:1\nT:T\n{}", body);
        let doc = abc::parse(&src);
        voices(&doc.tunes[0])
            .iter()
            .map(|v| {
                let events: Vec<String> = v
                    .events
                    .iter()
                    .map(|e| match e {
                        Event::Line(l) => {
                            let span = l[0].span.join(l[l.len() - 1].span);
                            src[span.start..span.end].trim().to_string()
                        }
                        Event::Field(f) => format!("{}:{}", f.name, f.value),
                    })
                    .collect();
                format!("{}: {}", v.id, events.join(" | "))
            })
            .collect()
    }

    #[test]
    fn a_tune_without_voices_has_one() {
        assert_eq!(outline("K:C\nabc\ndef\n"), vec!["1: abc | def"]);
        assert_eq!(outline("K:C\nabc\nK:G\ndef\nL:1/4\ngab\n"), vec!["1: abc | K:G | def | L:1/4 | gab"]);
        // a tune without music has no voices, even if it declares some
        assert!(outline("K:C\n").is_empty());
        assert!(outline("V:1\nV:2\nK:C\n").is_empty());
    }

    #[test]
    fn voices_written_one_after_the_other() {
        let body = "V:1 name=A\nV:2 clef=bass\nK:C\nV:1\nabc\ndef\nV:2\nABC\nDEF\n";
        assert_eq!(outline(body), vec!["1: abc | def", "2: ABC | DEF"]);
        let doc = abc::parse(&format!("X:1\nT:T\n{}", body));
        let v = voices(&doc.tunes[0]);
        assert_eq!(v[0].props.name.as_deref(), Some("A"));
        assert_eq!(v[1].props.clef, Some(Clef::Bass));
        assert!(v[0].span.is_some());
    }

    #[test]
    fn voices_written_line_by_line() {
        let body = "K:C\n[V:T] abc\n[V:B] ABC\n[V:T] def\n[V:B] DEF\n";
        assert_eq!(outline(body), vec!["T: abc | def", "B: ABC | DEF"]);
        // two voices on one line
        assert_eq!(outline("K:C\n[V:1]abc[V:2]ABC\n"), vec!["1: abc", "2: ABC"]);
        // a voice mark with nothing after it is not a line
        assert_eq!(outline("K:C\n[V:1] \n[V:2] ABC\n"), vec!["2: ABC"]);
    }

    #[test]
    fn music_before_a_voice_is_named() {
        // it belongs to the first voice of the header
        assert_eq!(outline("V:A\nV:B\nK:C\nabc\nV:B\nABC\n"), vec!["A: abc", "B: ABC"]);
        // or to voice 1 when the header declares none
        assert_eq!(outline("K:C\nabc\nV:2\nABC\nV:1\ndef\n"), vec!["1: abc | def", "2: ABC"]);
        // a declared voice is kept without music, an undeclared one is not
        assert_eq!(outline("V:A\nV:B\nK:C\nV:A\nabc\n"), vec!["A: abc", "B: "]);
        assert_eq!(outline("K:C\nV:2\nABC\n"), vec!["2: ABC"]);
    }

    #[test]
    fn body_fields_belong_to_their_voice() {
        // before a voice is named: every voice, also those still to come
        let body = "V:A\nK:C\nM:3/4\nV:A\nabc\nV:B\nABC\n";
        assert_eq!(outline(body), vec!["A: M:3/4 | abc", "B: M:3/4 | ABC"]);
        // after: the voice they are written in
        let body = "K:C\nV:A\nabc\nK:G\ndef\nV:B\nABC\nDEF\n";
        assert_eq!(outline(body), vec!["A: abc | K:G | def", "B: ABC | DEF"]);
    }

    #[test]
    fn continued_lines_are_one_line() {
        assert_eq!(outline("K:C\nabc\\\ndef\ngab\n"), vec!["1: abc\\\ndef | gab"]);
        // also when the voice is named again, but not into another voice
        assert_eq!(outline("K:C\n[V:1]abc\\\n[V:1]def\n"), vec!["1: abc\\\n[V:1]def"]);
        assert_eq!(outline("K:C\n[V:1]abc\\\n[V:2]ABC\n"), vec!["1: abc", "2: ABC"]);
        // K: and M: end a continued line; L: does not
        assert_eq!(outline("K:C\nabc\\\nK:G\ndef\n"), vec!["1: abc | K:G | def"]);
        assert_eq!(outline("K:C\nabc\\\nL:1/4\ndef\n"), vec!["1: L:1/4 | abc\\\nL:1/4\ndef"]);
    }

    #[test]
    fn lyrics_belong_to_the_music_before_them() {
        // (verses, symbols of music) for each block of each voice
        let blocks = |body: &str| -> Vec<Vec<(usize, usize)>> {
            let doc = abc::parse(&format!("X:1\nT:T\nK:C\n{}", body));
            voices(&doc.tunes[0])
                .iter()
                .map(|v| v.lyrics.iter().map(|b| (b.verses.len(), b.music.len())).collect())
                .collect()
        };
        assert_eq!(blocks("abc\nw:la la la\n"), vec![vec![(1, 3)]]);
        // lines right after each other are verses
        assert_eq!(blocks("abc\nw:la la la\nw:li li li\n"), vec![vec![(2, 3)]]);
        // lyrics after two lines of music, then more music with its own
        assert_eq!(blocks("abc\ndef\nw:la\ngab\nw:li\n"), vec![vec![(1, 6), (1, 3)]]);
        // each voice has its own
        assert_eq!(blocks("[V:1]abc\nw:la\n[V:2]ABCD\nw:li\nw:lo\n"), vec![vec![(1, 3)], vec![(2, 4)]]);
        // lyrics written at the end, voice after voice
        assert_eq!(blocks("[V:1]abc\n[V:2]ABCD\nV:1\nw:la\nV:2\nw:li\n"), vec![vec![(1, 3)], vec![(1, 4)]]);
    }
}
