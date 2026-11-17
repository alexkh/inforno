  //============================================================================
 // abc/src/document.rs
//==============================================================================

//! Splits ABC text into a file header, tunes and lines, and parses each music
//! line with the peg grammar. Parsing never fails: anything that cannot be
//! understood is kept with a [`Diagnostic`], and the rest of the file still parses.

use crate::datatypes::*;
use crate::grammar::abc;

/// Field letters that may appear on their own line inside a tune body.
const BODY_FIELDS: &str = "IKLMmNPQRrsTUVWw";

/// Parses a whole ABC file. Accepts `\n` and `\r\n` line endings.
pub fn parse(source: &str) -> Document {
    let mut p = Parser {
        doc: Document::default(),
        state: State::FileHeader,
        tune: None,
    };
    for (start, text) in lines(source) {
        p.line(start, text);
    }
    p.finish_tune(source.len());
    p.doc
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum State {
    /// Before the first tune or blank line.
    FileHeader,
    /// Between tunes: free text, ignored.
    Outside,
    /// After `X:`, before `K:`.
    TuneHeader,
    TuneBody,
}

struct Parser {
    doc: Document,
    state: State,
    tune: Option<Tune>,
}

impl Parser {
    fn line(&mut self, start: usize, text: &str) {
        let span = Span::new(start, start + text.len());

        if text.trim().is_empty() {
            // a blank line ends the file header or the current tune
            if self.tune.is_some() {
                self.finish_tune(start);
            }
            self.state = State::Outside;
            return;
        }

        if text.starts_with("X:") {
            // a new tune, even without a blank line before it
            self.finish_tune(start);
            let field = field(text, span);
            self.tune = Some(Tune {
                span,
                header: vec![field],
                directives: vec![],
                body: vec![],
            });
            self.state = State::TuneHeader;
            return;
        }

        match self.state {
            State::FileHeader => {
                if is_field(text) {
                    self.doc.file_header.push(field(text, span));
                } else if let Some(d) = directive(text, span) {
                    self.doc.file_directives.push(d);
                }
            }
            State::Outside => {}
            State::TuneHeader => {
                if text.starts_with('%') {
                    if let Some(d) = directive(text, span) {
                        self.tune_mut().directives.push(d);
                    }
                    return;
                }
                if text.starts_with("+:") {
                    continue_field(self.tune_mut().header.last_mut(), text, span);
                    return;
                }
                if is_field(text) {
                    let f = field(text, span);
                    let is_key = f.name == 'K';
                    self.tune_mut().header.push(f);
                    if is_key {
                        self.state = State::TuneBody;
                    }
                    return;
                }
                // music before K:; be lenient and treat the header as finished
                self.doc
                    .diagnostics
                    .push(Diagnostic::new(span, "music before the K: field"));
                self.state = State::TuneBody;
                self.body_line(start, text, span);
            }
            State::TuneBody => self.body_line(start, text, span),
        }
    }

    fn body_line(&mut self, start: usize, text: &str, span: Span) {
        let line = if let Some(d) = directive(text, span) {
            BodyLine::Directive(d)
        } else if text.starts_with('%') {
            BodyLine::Comment(span)
        } else if text.starts_with("+:") {
            match self.tune_mut().body.last_mut() {
                Some(BodyLine::Field(f)) => continue_field(Some(f), text, span),
                Some(BodyLine::Lyrics(l)) => {
                    l.tokens.extend(lyric_tokens(start, text));
                    l.span = l.span.join(span);
                }
                _ => {}
            }
            return;
        } else if text.starts_with("w:") {
            BodyLine::Lyrics(LyricLine {
                span,
                tokens: lyric_tokens(start, text),
            })
        } else if is_body_field(text) {
            BodyLine::Field(field(text, span))
        } else {
            BodyLine::Music(self.music_line(start, text, span))
        };
        self.tune_mut().body.push(line);
    }

    fn music_line(&mut self, start: usize, text: &str, span: Span) -> MusicLine {
        let music = &text[..comment_start(text)];
        let trimmed = music.trim_end();
        let (music, continues) = match trimmed.strip_suffix('\\') {
            Some(m) => (m, true),
            None => (music, false),
        };

        let mut symbols = abc::music_items(music).unwrap_or_else(|_| {
            // music_items accepts any text; keep the whole line if it ever fails
            vec![Symbol::new(
                Span::new(0, music.len()),
                MusicSymbol::Unknown(music.to_string()),
            )]
        });
        for s in symbols.iter_mut() {
            s.span = s.span.offset(start);
            match &mut s.kind {
                MusicSymbol::InlineField(f) => f.span = f.span.offset(start),
                MusicSymbol::Unknown(u) => self.doc.diagnostics.push(Diagnostic::new(
                    s.span,
                    format!("cannot read `{}`", u),
                )),
                _ => {}
            }
        }
        MusicLine {
            span,
            symbols,
            continues,
        }
    }

    fn tune_mut(&mut self) -> &mut Tune {
        self.tune.as_mut().expect("inside a tune")
    }

    /// Closes the current tune; `end` is where its text ends.
    fn finish_tune(&mut self, end: usize) {
        if let Some(mut tune) = self.tune.take() {
            if self.state == State::TuneHeader {
                let span = tune.header[0].span;
                self.doc
                    .diagnostics
                    .push(Diagnostic::new(span, "tune has no K: field"));
            }
            // end of the last line that belongs to the tune, without the newline
            let last = tune
                .body
                .last()
                .map(body_line_span)
                .or_else(|| tune.header.last().map(|f| f.span))
                .unwrap_or(tune.span);
            tune.span = Span::new(tune.span.start, last.end.min(end).max(tune.span.start));
            self.doc.tunes.push(tune);
        }
    }
}

fn body_line_span(l: &BodyLine) -> Span {
    match l {
        BodyLine::Music(m) => m.span,
        BodyLine::Field(f) => f.span,
        BodyLine::Lyrics(l) => l.span,
        BodyLine::Comment(s) => *s,
        BodyLine::Directive(d) => d.span,
    }
}

/// Yields `(byte offset, line text)` without `\n` or `\r\n`.
fn lines(source: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut start = 0;
    source.split_inclusive('\n').map(move |raw| {
        let line_start = start;
        start += raw.len();
        let text = raw.strip_suffix('\n').unwrap_or(raw);
        let text = text.strip_suffix('\r').unwrap_or(text);
        (line_start, text)
    })
}

/// `T:...`, `w:...`, `+:...`
fn is_field(text: &str) -> bool {
    let mut c = text.chars();
    matches!((c.next(), c.next()), (Some(n), Some(':')) if n.is_ascii_alphabetic() || n == '+')
}

fn is_body_field(text: &str) -> bool {
    let mut c = text.chars();
    matches!((c.next(), c.next()), (Some(n), Some(':')) if BODY_FIELDS.contains(n))
}

/// Byte index of the `%` that starts a comment, or the length of the text.
/// A `\%` is an escaped percent sign, and a `%` inside `"..."` is text.
fn comment_start(text: &str) -> usize {
    let mut in_quotes = false;
    let mut prev = '\0';
    for (i, c) in text.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            '%' if !in_quotes && prev != '\\' => return i,
            _ => {}
        }
        prev = c;
    }
    text.len()
}

fn field(text: &str, span: Span) -> InfoField {
    let name = text.chars().next().unwrap_or('?');
    let value = &text[2..comment_start(text)];
    InfoField::new(name, value.trim(), span)
}

/// `%%name value`; `None` for any other line.
fn directive(text: &str, span: Span) -> Option<Directive> {
    let rest = text.strip_prefix("%%")?.trim();
    let (name, value) = match rest.split_once(char::is_whitespace) {
        Some((name, value)) => (name, value.trim()),
        None => (rest, ""),
    };
    Some(Directive {
        span,
        name: name.to_string(),
        value: value.to_string(),
    })
}

/// The tokens of a `w:` line, or of a `+:` line continuing one, that starts at
/// byte `start` of the source.
fn lyric_tokens(start: usize, text: &str) -> Vec<LyricToken> {
    let value = &text[2..comment_start(text)];
    // lyric_items accepts any text
    let mut tokens = abc::lyric_items(value).unwrap_or_default();
    for t in tokens.iter_mut() {
        t.span = t.span.offset(start + 2);
    }
    tokens
}

/// `+:` continues the previous field's value.
fn continue_field(prev: Option<&mut InfoField>, text: &str, span: Span) {
    if let Some(f) = prev {
        let more = text[2..comment_start(text)].trim();
        if !more.is_empty() {
            if !f.value.is_empty() {
                f.value.push(' ');
            }
            f.value.push_str(more);
        }
        f.span = f.span.join(span);
    }
}
