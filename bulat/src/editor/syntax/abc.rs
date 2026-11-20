//! ABC music notation (abcnotation.com, standard 2.1), for highlighting:
//! enough to tell the parts of a tune apart at a glance, not a parser that
//! checks anything. Every byte of the text ends up in exactly one token,
//! since the editor lays out the text from the tokens.
//!
//! ABC is line-based: a line starting with a letter and a colon is a field
//! (`T:` title, `K:` key, `w:` lyrics ...), `%` starts a comment, `%%` a
//! directive, and every other line is music.

use std::collections::BTreeSet;

use crate::editor::Token;
use crate::editor::syntax::{Syntax, TokenType};

pub fn parse(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        lex_line(body, &mut tokens);
        push(&mut tokens, TokenType::Whitespace('\n'), &line[body.len()..]);
    }
    tokens
}

fn push(tokens: &mut Vec<Token>, ty: TokenType, s: &str) {
    if !s.is_empty() {
        tokens.push(Token::new(ty, s));
    }
}

/// One line, without its line break.
fn lex_line(line: &str, tokens: &mut Vec<Token>) {
    if line.starts_with("%%") {
        return push(tokens, TokenType::Special, line);
    }
    if line.starts_with('%') {
        return push(tokens, TokenType::Comment(false), line);
    }
    // A field: a letter and a colon at the start of the line, or `+:`
    // continuing the field before. The rest of the line is its value, up to
    // a comment.
    let b = line.as_bytes();
    if b.len() >= 2 && b[1] == b':' && (b[0].is_ascii_alphabetic() || b[0] == b'+') {
        push(tokens, TokenType::Keyword, &line[..2]);
        let (value, comment) = split_comment(&line[2..]);
        push(tokens, field_value_type(b[0]), value);
        push(tokens, TokenType::Comment(false), comment);
        return;
    }
    lex_music(line, tokens);
}

/// How a field's value is colored. Text meant to be read (lyrics, title,
/// composer, notes, and `+:` continuing any of them) in the plain text
/// color; values that set how the music is read (tune number, key, meter,
/// unit length, tempo, voice, parts, instructions, symbols) as strings, so
/// they stand out.
fn field_value_type(field: u8) -> TokenType {
    match field {
        b'X' | b'K' | b'M' | b'L' | b'Q' | b'V' | b'P' | b'I' | b'U' | b'm' | b's' => TokenType::Str('"'),
        _ => TokenType::Literal,
    }
}

/// `s` split at its first `%` that isn't escaped as `\%`.
fn split_comment(s: &str) -> (&str, &str) {
    let mut prev = '\0';
    for (i, c) in s.char_indices() {
        if c == '%' && prev != '\\' {
            return s.split_at(i);
        }
        prev = c;
    }
    (s, "")
}

/// A line of music. Every branch takes at least one character.
fn lex_music(line: &str, tokens: &mut Vec<Token>) {
    let mut rest = line;
    while let Some(c) = rest.chars().next() {
        let b = rest.as_bytes();
        let (ty, len) = match c {
            '%' => (TokenType::Comment(false), rest.len()),
            ' ' | '\t' => (TokenType::Whitespace(c), ascii_run(rest, |x| x == b' ' || x == b'\t')),
            // an annotation, "^above" (^ _ < > @ say where it goes): plain
            // text; otherwise a chord symbol, "Am7"
            '"' if b.get(1).is_some_and(|x| b"^_<>@".contains(x)) => (TokenType::Literal, closed_by(rest, '"')),
            '"' => (TokenType::Str('"'), closed_by(rest, '"')),
            // a decoration: !trill!, !p!, or the older +trill+
            '!' | '+' if rest[1..].contains(c) => (TokenType::Special, closed_by(rest, c)),
            // an inline field: [K:D], [M:3/4]
            '[' if b.len() >= 3 && b[1].is_ascii_alphabetic() && b[2] == b':' => {
                (TokenType::Keyword, closed_by(rest, ']'))
            }
            // a variant ending, [1 or [2,3, or the [| bar line
            '[' if b.get(1).is_some_and(|x| x.is_ascii_digit() || *x == b'|') => {
                (TokenType::Punctuation('|'), 1 + bar_len(&rest[1..]))
            }
            '|' | ':' => (TokenType::Punctuation('|'), bar_len(rest)),
            // a note: accidentals, the pitch, octave marks
            '^' | '_' | '=' | 'A'..='G' | 'a'..='g' => match note_len(rest) {
                0 => (TokenType::Punctuation(c), 1),
                n => (TokenType::Function, n),
            },
            // rests, invisible rests, and their whole-measure forms
            'z' | 'x' | 'Z' | 'X' => (TokenType::Type, 1),
            // a length: 2, /2, 3/2, //
            '0'..='9' | '/' => (
                TokenType::Numeric(false),
                ascii_run(rest, |x| x.is_ascii_digit() || x == b'/'),
            ),
            // a tuplet, (3 or (3:2:3; otherwise ( starts a slur
            '(' if b.get(1).is_some_and(u8::is_ascii_digit) => (
                TokenType::Special,
                1 + ascii_run(&rest[1..], |x| x.is_ascii_digit() || x == b':'),
            ),
            // decoration shorthands: ~ roll, . staccato, H fermata, T trill,
            // u up-bow, v down-bow, and the rest of the standard's set
            '~' | '.' | 'H' | 'L' | 'M' | 'O' | 'P' | 'S' | 'T' | 'u' | 'v' => (TokenType::Special, 1),
            '(' | ')' | '[' | ']' | '{' | '}' | '-' | '<' | '>' | '&' | '\\' | ',' | '\'' | '!' | '+' => {
                (TokenType::Punctuation(c), 1)
            }
            _ => (TokenType::Literal, c.len_utf8()),
        };
        let (token, after) = rest.split_at(len);
        push(tokens, ty, token);
        rest = after;
    }
}

/// Length of the run of ASCII bytes at the start of `s` that match `f`
/// (so it always ends on a character boundary).
fn ascii_run(s: &str, f: impl Fn(u8) -> bool) -> usize {
    s.bytes().take_while(|&x| f(x)).count()
}

/// Length of `s` up to and including the first `close` after its first
/// character, or all of `s` when there's none.
fn closed_by(s: &str, close: char) -> usize {
    let first = s.chars().next().map_or(0, char::len_utf8);
    s[first..].find(close).map_or(s.len(), |i| first + i + close.len_utf8())
}

/// Length of a bar line at the start of `s` (|, ||, |], |:, :|, ::, :|])
/// with the variant ending that may follow it (|1, :|2, |1,3).
fn bar_len(s: &str) -> usize {
    let mut n = ascii_run(s, |x| x == b'|' || x == b':');
    if s.as_bytes().get(n) == Some(&b']') {
        n += 1;
    } else if s.as_bytes().get(n).is_some_and(u8::is_ascii_digit) {
        n += ascii_run(&s[n..], |x| x.is_ascii_digit() || x == b',' || x == b'-');
    }
    n
}

/// Length of a note at the start of `s`: accidentals (^, ^^, _, __, =), the
/// pitch letter and octave marks (, and '). 0 when no pitch letter follows
/// the accidentals.
fn note_len(s: &str) -> usize {
    let acc = ascii_run(s, |x| x == b'^' || x == b'_' || x == b'=');
    match s.as_bytes().get(acc) {
        Some(b'A'..=b'G' | b'a'..=b'g') => {
            let n = acc + 1;
            n + ascii_run(&s[n..], |x| x == b',' || x == b'\'')
        }
        _ => 0,
    }
}

impl Syntax {
    pub fn abc() -> Self {
        Syntax {
            language: "ABC",
            case_sensitive: true,
            comment: "%",
            comment_multiline: ["", ""],
            hyperlinks: BTreeSet::new(),
            keywords: BTreeSet::new(),
            types: BTreeSet::new(),
            special: BTreeSet::new(),
            dynamic_rules: None,
            native_parser: Some(parse),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn every_byte_is_in_a_token() {
        let tune = "X:1\nT:Speed the Plough % trad.\nM:4/4\nK:G\r\n\
                    |:\"G\"GABG ^c2d>e|[K:D](3fga !trill!b2 z2|[1 [CEG]4:|2 g4|]\n\
                    w:ly-rics é\n%%score 1\n% the end";
        let joined: String = parse(tune).iter().map(|t| t.buffer()).collect();
        assert_eq!(joined, tune);
    }
}
