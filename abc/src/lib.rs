  //============================================================================
 // abc/src/lib.rs
//==============================================================================

//! # ABC Parser (WIP)
//!
//! An ABC music notation parser for Rust based on the
//! [2.1 Standard](http://abcnotation.com/wiki/abc:standard:v2.1). This crate is currently a work
//! in progress and not fully compliant. For full documentation on which parts of the standard
//! are implemented, take a look at the tests.
//!
//! Parse a file with [`parse`]. It never fails: text it cannot read is reported in
//! [`datatypes::Document::diagnostics`] and the rest of the file still parses.
//!
//! ```
//! use abc::datatypes::*;
//!
//! let doc = abc::parse("X:1\nT:Example\nK:D\nA2 B|\n");
//! let tune = &doc.tunes[0];
//! assert_eq!(tune.field('T'), Some("Example"));
//! let line = tune.music_lines().next().unwrap();
//! assert_eq!(line.symbols.len(), 4);
//! assert!(doc.diagnostics.is_empty());
//! ```

pub mod datatypes;
pub mod document;
mod grammar;

pub use document::parse;
pub use grammar::abc;

#[cfg(test)]
mod tests {
    use super::*;
    use datatypes::*;
    use num_rational::Rational32;

    /// The symbol kinds of one music line.
    fn kinds(s: &str) -> Vec<MusicSymbol> {
        abc::music_items(s)
            .unwrap()
            .into_iter()
            .map(|s| s.kind)
            .collect()
    }

    fn has_unknown(s: &str) -> bool {
        kinds(s).iter().any(|k| matches!(k, MusicSymbol::Unknown(_)))
    }

    fn pitch(note: Note) -> Pitch {
        Pitch::new(note, 1, None, None)
    }

    fn n(note: Note, octave: i8, length: i32) -> MusicSymbol {
        MusicSymbol::new_note(vec![], None, note, octave, length, None)
    }

    fn tied(note: Note, octave: i8, length: i32, tie: Tie) -> MusicSymbol {
        MusicSymbol::new_note(vec![], None, note, octave, length, Some(tie))
    }

    fn bar(b: &str) -> MusicSymbol {
        MusicSymbol::Bar(b.to_string())
    }

    fn field_values(fields: &[InfoField]) -> Vec<(char, &str)> {
        fields.iter().map(|f| (f.name, f.value.as_str())).collect()
    }

    //--------------------------------------------------------------------------
    // documents

    #[test]
    fn empty_document() {
        let d = parse("");
        assert!(d.file_header.is_empty());
        assert!(d.tunes.is_empty());
        assert!(d.diagnostics.is_empty());
    }

    #[test]
    fn tune_header() {
        let d = parse("X:1\nT:Some Title\nK:G\n");
        assert_eq!(d.tunes.len(), 1);
        assert_eq!(
            field_values(&d.tunes[0].header),
            vec![('X', "1"), ('T', "Some Title"), ('K', "G")]
        );
        assert!(d.tunes[0].body.is_empty());
        assert!(d.diagnostics.is_empty());
    }

    #[test]
    fn tune_header_unicode() {
        let d = parse("X:1\nT:S©ome ©Title©\nK:G\n");
        assert_eq!(d.tunes[0].field('T'), Some("S©ome ©Title©"));
    }

    #[test]
    fn tune_header_extra_fields() {
        let d = parse("X:1\nT:Some Title\nO:England\nK:G\n");
        assert_eq!(
            field_values(&d.tunes[0].header),
            vec![('X', "1"), ('T', "Some Title"), ('O', "England"), ('K', "G")]
        );
    }

    #[test]
    fn field_comment_and_spaces() {
        let d = parse("X: 1\nT:  Title  % a comment\nK:D\n");
        assert_eq!(d.tunes[0].field('X'), Some("1"));
        assert_eq!(d.tunes[0].field('T'), Some("Title"));
    }

    #[test]
    fn field_continuation() {
        let d = parse("X:1\nT:Long\n+:Title\nK:D\n");
        assert_eq!(d.tunes[0].field('T'), Some("Long Title"));
        assert_eq!(d.tunes[0].header.len(), 3);
    }

    #[test]
    fn file_header() {
        let d = parse("M:4/4\nO:Irish\n\nX:1\nT:Some Title\nK:G\nabc\n");
        assert_eq!(field_values(&d.file_header), vec![('M', "4/4"), ('O', "Irish")]);
        assert_eq!(d.tunes.len(), 1);
        assert_eq!(d.tunes[0].music_lines().count(), 1);
    }

    #[test]
    fn lone_tune_then_blank_line() {
        let d = parse("X:1\nT:A\nK:G\n\n");
        assert!(d.file_header.is_empty());
        assert_eq!(d.tunes.len(), 1);
    }

    #[test]
    fn multiple_tunes_blank_lines() {
        let d = parse("X:1\nT:A\nK:G\nabc\n\n\nX:2\nT:B\nK:D\ndef\n");
        assert!(d.file_header.is_empty());
        assert_eq!(d.tunes.len(), 2);
        assert_eq!(d.tunes[1].field('T'), Some("B"));
    }

    #[test]
    fn multiple_tunes_no_blank_line() {
        let d = parse("X:1\nT:A\nK:G\nabc\nX:2\nT:B\nK:D\ndef\n");
        assert_eq!(d.tunes.len(), 2);
        assert_eq!(d.tunes[0].music_lines().count(), 1);
    }

    #[test]
    fn leading_blank_lines() {
        let d = parse("\n\nX:1\nT:A\nK:G\nabc\n");
        assert_eq!(d.tunes.len(), 1);
    }

    #[test]
    fn free_text_after_tune_is_ignored() {
        let d = parse("X:1\nT:A\nK:G\nabc\n\nsome text here\n");
        assert_eq!(d.tunes.len(), 1);
        assert!(d.diagnostics.is_empty());
    }

    #[test]
    fn no_trailing_newline() {
        let d = parse("X:1\nT:A\nK:G\nabc");
        assert_eq!(d.tunes[0].music_lines().count(), 1);
    }

    #[test]
    fn crlf() {
        let src = "X:1\r\nT:A\r\nK:G\r\nabc\r\n";
        let d = parse(src);
        assert_eq!(d.tunes[0].field('T'), Some("A"));
        let line = d.tunes[0].music_lines().next().unwrap();
        assert_eq!(&src[line.span.start..line.span.end], "abc");
        assert_eq!(line.symbols.len(), 3);
        assert!(d.diagnostics.is_empty());
    }

    #[test]
    fn spans_point_into_source() {
        let src = "X:1\nT:A\nK:G\neg|a2-ab \"Am\"c\n";
        let d = parse(src);
        let line = d.tunes[0].music_lines().next().unwrap();
        let texts: Vec<&str> = line
            .symbols
            .iter()
            .map(|s| &src[s.span.start..s.span.end])
            .collect();
        assert_eq!(texts, vec!["e", "g", "|", "a2-", "a", "b", " ", "\"Am\"", "c"]);
        assert_eq!(&src[d.tunes[0].header[1].span.start..d.tunes[0].header[1].span.end], "T:A");
    }

    #[test]
    fn tune_span_and_lookup() {
        let src = "X:1\nT:A\nK:G\nabc\n\nX:2\nT:B\nK:D\ndef\n";
        let d = parse(src);
        assert_eq!(&src[d.tunes[0].span.start..d.tunes[0].span.end], "X:1\nT:A\nK:G\nabc");
        let pos = src.find("def").unwrap();
        assert_eq!(d.tune_at(pos).and_then(|t| t.field('T')), Some("B"));
        assert!(d.tune_at(src.find("\n\n").unwrap() + 1).is_none());
    }

    #[test]
    fn body_lines() {
        let src = "X:1\nT:A\nK:G\nabc % note\n% comment\n%%scale 0.8\nK:D\nw:la la la\ndef\n";
        let d = parse(src);
        let body = &d.tunes[0].body;
        assert_eq!(body.len(), 6);
        assert!(matches!(&body[0], BodyLine::Music(m) if m.symbols.len() == 4));
        assert!(matches!(&body[1], BodyLine::Comment(_)));
        assert!(matches!(&body[2], BodyLine::Directive(d) if d.name == "scale" && d.value == "0.8"));
        assert!(matches!(&body[3], BodyLine::Field(f) if f.name == 'K' && f.value == "D"));
        assert!(matches!(&body[4], BodyLine::Lyrics(l) if l.tokens.len() == 3));
        assert!(matches!(&body[5], BodyLine::Music(_)));
        assert!(d.diagnostics.is_empty());
    }

    #[test]
    fn line_continuation() {
        let d = parse("X:1\nT:A\nK:G\nabc \\\ndef\n");
        let lines: Vec<&MusicLine> = d.tunes[0].music_lines().collect();
        assert!(lines[0].continues);
        assert!(!lines[1].continues);
        assert!(!lines[0]
            .symbols
            .iter()
            .any(|s| matches!(s.kind, MusicSymbol::Unknown(_))));
    }

    #[test]
    fn music_line_starting_like_a_field() {
        // "c" is not a body field, so this is the note c followed by a repeat bar
        let d = parse("X:1\nT:A\nK:G\nc:|\n");
        let line = d.tunes[0].music_lines().next().unwrap();
        assert_eq!(line.symbols.len(), 2);
    }

    #[test]
    fn missing_key_field() {
        let d = parse("X:1\nT:A\n");
        assert_eq!(d.tunes.len(), 1);
        assert_eq!(d.diagnostics.len(), 1);
    }

    #[test]
    fn music_before_key_field() {
        let d = parse("X:1\nT:A\nabc\n");
        assert_eq!(d.tunes[0].music_lines().count(), 1);
        assert_eq!(d.diagnostics.len(), 1);
    }

    #[test]
    fn unknown_text_is_reported_and_skipped() {
        let src = "X:1\nT:A\nK:G\nab ??| cd\n";
        let d = parse(src);
        assert_eq!(d.diagnostics.len(), 1);
        let span = d.diagnostics[0].span;
        assert_eq!(&src[span.start..span.end], "??");
        let line = d.tunes[0].music_lines().next().unwrap();
        assert_eq!(line.symbols.len(), 8);
    }

    #[test]
    fn inline_field_span() {
        let src = "X:1\nT:A\nK:G\nA[M:3/4]B\n";
        let d = parse(src);
        let line = d.tunes[0].music_lines().next().unwrap();
        match &line.symbols[1].kind {
            MusicSymbol::InlineField(f) => {
                assert_eq!(f.name, 'M');
                assert_eq!(f.value, "3/4");
                assert_eq!(&src[f.span.start..f.span.end], "[M:3/4]");
            }
            k => panic!("expected an inline field, got {:?}", k),
        }
    }

    //--------------------------------------------------------------------------
    // directives

    #[test]
    fn directives_are_kept() {
        let src = "%%pagewidth 21cm\nM:4/4\n\nX:1\n%%score (1 2)\n% a comment\nT:A\nK:G\nabc\n%%vskip\n";
        let d = parse(src);
        assert_eq!(d.file_directives.len(), 1);
        assert_eq!(d.file_directives[0].name, "pagewidth");
        assert_eq!(d.file_directives[0].value, "21cm");
        let tune = &d.tunes[0];
        assert_eq!(tune.directives.len(), 1);
        assert_eq!(tune.directive("score"), Some("(1 2)"));
        assert_eq!(tune.directive("staves"), None);
        let span = tune.directives[0].span;
        assert_eq!(&src[span.start..span.end], "%%score (1 2)");
        // directives do not count as header fields
        assert_eq!(field_values(&tune.header), vec![('X', "1"), ('T', "A"), ('K', "G")]);
        assert!(matches!(&tune.body[1], BodyLine::Directive(d) if d.name == "vskip" && d.value.is_empty()));
        assert!(d.diagnostics.is_empty());
    }

    #[test]
    fn last_header_directive_wins() {
        let d = parse("X:1\n%%score 1 2\n%%score 2 1\nK:C\n");
        assert_eq!(d.tunes[0].directive("score"), Some("2 1"));
    }

    //--------------------------------------------------------------------------
    // lyrics

    fn lyrics(s: &str) -> Vec<Lyric> {
        abc::lyric_items(s).unwrap().into_iter().map(|t| t.kind).collect()
    }

    fn syl(text: &str) -> Lyric {
        Lyric::Syllable { text: text.to_string(), hyphen: false }
    }

    /// A syllable followed by a hyphen.
    fn syl_(text: &str) -> Lyric {
        Lyric::Syllable { text: text.to_string(), hyphen: true }
    }

    #[test]
    fn lyric_words() {
        assert_eq!(lyrics("la la la"), vec![syl("la"), syl("la"), syl("la")]);
        assert_eq!(lyrics("  la\tla  "), vec![syl("la"), syl("la")]);
        assert!(lyrics("").is_empty());
        assert!(lyrics("   ").is_empty());
    }

    #[test]
    fn lyric_hyphens() {
        assert_eq!(lyrics("syll-a-ble"), vec![syl_("syll"), syl_("a"), syl("ble")]);
        // a hyphen after a hyphen or a space is a syllable of its own
        assert_eq!(lyrics("go--ing"), vec![syl_("go"), syl_(""), syl("ing")]);
        assert_eq!(lyrics("a - b"), vec![syl("a"), syl_(""), syl("b")]);
    }

    #[test]
    fn lyric_holds_skips_and_bars() {
        assert_eq!(lyrics("time__"), vec![syl("time"), Lyric::Hold, Lyric::Hold]);
        assert_eq!(lyrics("time _ _"), vec![syl("time"), Lyric::Hold, Lyric::Hold]);
        assert_eq!(lyrics("syll-_able"), vec![syl_("syll"), Lyric::Hold, syl("able")]);
        assert_eq!(lyrics("la * la"), vec![syl("la"), Lyric::Skip, syl("la")]);
        assert_eq!(lyrics("la|la | la"), vec![syl("la"), Lyric::Bar, syl("la"), Lyric::Bar, syl("la")]);
    }

    #[test]
    fn lyric_escapes() {
        assert_eq!(lyrics("of~the day"), vec![syl("of the"), syl("day")]);
        assert_eq!(lyrics("1.~Three"), vec![syl("1. Three")]);
        assert_eq!(lyrics("well\\-known"), vec![syl("well-known")]);
        // any other backslash is kept
        assert_eq!(lyrics("a\\b"), vec![syl("a\\b")]);
    }

    #[test]
    fn lyric_spans_point_into_source() {
        let src = "X:1\nT:A\nK:G\nabcd\nw: Hé, the dus-ty_ % sung\n";
        let d = parse(src);
        let BodyLine::Lyrics(l) = &d.tunes[0].body[1] else {
            panic!("expected lyrics");
        };
        let texts: Vec<&str> = l.tokens.iter().map(|t| &src[t.span.start..t.span.end]).collect();
        assert_eq!(texts, vec!["Hé,", "the", "dus-", "ty", "_"]);
        assert_eq!(&src[l.span.start..l.span.end], "w: Hé, the dus-ty_ % sung");
        assert!(d.diagnostics.is_empty());
    }

    #[test]
    fn lyric_continuation() {
        let src = "X:1\nT:A\nK:G\nabcd\nw:la la\n+:li lo\n";
        let d = parse(src);
        let body = &d.tunes[0].body;
        assert_eq!(body.len(), 2);
        let BodyLine::Lyrics(l) = &body[1] else {
            panic!("expected lyrics");
        };
        assert_eq!(l.tokens.len(), 4);
        assert_eq!(&src[l.tokens[3].span.start..l.tokens[3].span.end], "lo");
        assert_eq!(&src[l.span.start..l.span.end], "w:la la\n+:li lo");
    }

    #[test]
    fn lyric_verses_and_words() {
        // adjacent w: lines are verses; W: stays an ordinary field
        let d = parse("X:1\nT:A\nK:G\nabcd\nw:one\nw:two\nW:words below\n");
        let body = &d.tunes[0].body;
        assert!(matches!(&body[1], BodyLine::Lyrics(l) if l.tokens.len() == 1));
        assert!(matches!(&body[2], BodyLine::Lyrics(l) if l.tokens.len() == 1));
        assert!(matches!(&body[3], BodyLine::Field(f) if f.name == 'W' && f.value == "words below"));
    }

    //--------------------------------------------------------------------------
    // music lines

    #[test]
    fn music_line_empty() {
        assert!(kinds("").is_empty());
    }

    #[test]
    fn music_line() {
        assert_eq!(kinds("A"), vec![MusicSymbol::note(Note::A)]);
    }

    #[test]
    fn music_line_multiple_notes() {
        assert_eq!(
            kinds("AB cd"),
            vec![
                MusicSymbol::note(Note::A),
                MusicSymbol::note(Note::B),
                MusicSymbol::VisualBreak,
                n(Note::C, 2, 1),
                n(Note::D, 2, 1),
            ]
        )
    }

    #[test]
    fn spans() {
        let items = abc::music_items("A2 B|").unwrap();
        let spans: Vec<Span> = items.iter().map(|s| s.span).collect();
        assert_eq!(
            spans,
            vec![Span::new(0, 2), Span::new(2, 3), Span::new(3, 4), Span::new(4, 5)]
        );
    }

    #[test]
    fn music_line_chord() {
        assert_eq!(
            kinds("!f! [F=AC]3/2[GBD] H[CEG]2"),
            vec![
                MusicSymbol::Stem {
                    pitches: vec![
                        pitch(Note::F),
                        Pitch::new(Note::A, 1, Some(Accidental::Natural), None),
                        pitch(Note::C)
                    ],
                    length: Rational32::new(3, 2),
                    decorations: vec![Decoration::Unresolved("f".to_string())],
                    tie: None
                },
                MusicSymbol::Stem {
                    pitches: vec![pitch(Note::G), pitch(Note::B), pitch(Note::D)],
                    length: Rational32::from_integer(1),
                    decorations: vec![],
                    tie: None
                },
                MusicSymbol::VisualBreak,
                MusicSymbol::Stem {
                    pitches: vec![pitch(Note::C), pitch(Note::E), pitch(Note::G)],
                    length: Rational32::from_integer(2),
                    decorations: vec![Decoration::Fermata],
                    tie: None
                }
            ]
        );
    }

    #[test]
    fn music_line_backticks() {
        assert_eq!(
            kinds("A2``B``C"),
            vec![
                MusicSymbol::note_from_length(Note::A, Rational32::from_integer(2)),
                MusicSymbol::note(Note::B),
                MusicSymbol::note(Note::C)
            ]
        )
    }

    #[test]
    fn tie() {
        assert_eq!(
            kinds("A-A"),
            vec![tied(Note::A, 1, 1, Tie::Solid), n(Note::A, 1, 1)]
        );
    }

    #[test]
    fn tie_across_bars() {
        assert_eq!(
            kinds("abc-|cba"),
            vec![
                n(Note::A, 2, 1),
                n(Note::B, 2, 1),
                tied(Note::C, 2, 1, Tie::Solid),
                bar("|"),
                n(Note::C, 2, 1),
                n(Note::B, 2, 1),
                n(Note::A, 2, 1),
            ]
        );
    }

    #[test]
    fn tie_with_length() {
        assert_eq!(
            kinds("c4-c4"),
            vec![tied(Note::C, 2, 4, Tie::Solid), n(Note::C, 2, 4)]
        );
    }

    #[test]
    fn tie_dotted() {
        assert_eq!(
            kinds("C.-C"),
            vec![tied(Note::C, 1, 1, Tie::Dotted), n(Note::C, 1, 1)]
        );
    }

    #[test]
    fn invalid_tie() {
        assert!(has_unknown("c4 -c4"));
        assert!(has_unknown("abc|-cba"));
    }

    #[test]
    fn endings_general() {
        assert_eq!(
            kinds("f|[1 d:|[2 d2 B|]"),
            vec![
                n(Note::F, 2, 1),
                bar("|"),
                MusicSymbol::Ending(1),
                MusicSymbol::VisualBreak,
                n(Note::D, 2, 1),
                bar(":|"),
                MusicSymbol::Ending(2),
                MusicSymbol::VisualBreak,
                n(Note::D, 2, 2),
                MusicSymbol::VisualBreak,
                MusicSymbol::note(Note::B),
                bar("|]")
            ]
        );
    }

    #[test]
    fn endings_1() {
        assert_eq!(abc::ending("[1").unwrap(), MusicSymbol::Ending(1))
    }
    #[test]
    fn endings_2() {
        assert_eq!(abc::ending("[2").unwrap(), MusicSymbol::Ending(2))
    }
    #[test]
    fn endings_3() {
        assert_eq!(abc::ending("[87654").unwrap(), MusicSymbol::Ending(87654))
    }

    #[test]
    fn ending_after_bar() {
        assert_eq!(kinds("|[1"), vec![bar("|"), MusicSymbol::Ending(1)])
    }

    #[test]
    fn ending_shorthand() {
        assert_eq!(
            kinds("f|1 d:|2 d2|]"),
            vec![
                n(Note::F, 2, 1),
                bar("|"),
                MusicSymbol::Ending(1),
                MusicSymbol::VisualBreak,
                n(Note::D, 2, 1),
                bar(":|"),
                MusicSymbol::Ending(2),
                MusicSymbol::VisualBreak,
                n(Note::D, 2, 2),
                bar("|]")
            ]
        );
    }

    #[test]
    fn ending_shorthand_spans() {
        let items = abc::music_items(":|2").unwrap();
        assert_eq!(items[0].span, Span::new(0, 2));
        assert_eq!(items[1].span, Span::new(2, 3));
    }

    #[test]
    fn ending_shorthand_needs_bar() {
        // a bare number is only an ending right after a bar line ending in '|'
        assert!(has_unknown("|:2"));
        assert!(has_unknown("|]2"));
        assert!(has_unknown("A 2"));
    }

    #[test]
    fn chord_1() {
        assert_eq!(
            abc::music_symbol("[CEG]").unwrap(),
            MusicSymbol::Stem {
                pitches: vec![pitch(Note::C), pitch(Note::E), pitch(Note::G)],
                length: Rational32::from_integer(1),
                decorations: vec![],
                tie: None
            }
        );
    }

    #[test]
    fn chord_2() {
        // lengths inside a chord: the first note decides, times the outer length
        assert_eq!(
            abc::music_symbol("[C2E2G2]3").unwrap(),
            MusicSymbol::Stem {
                pitches: vec![pitch(Note::C), pitch(Note::E), pitch(Note::G)],
                length: Rational32::from_integer(6),
                decorations: vec![],
                tie: None
            }
        );
    }

    #[test]
    fn chord_inner_ties() {
        assert_eq!(
            abc::music_symbol("[C-EG]").unwrap(),
            MusicSymbol::Stem {
                pitches: vec![
                    Pitch::new(Note::C, 1, None, Some(Tie::Solid)),
                    pitch(Note::E),
                    pitch(Note::G)
                ],
                length: Rational32::from_integer(1),
                decorations: vec![],
                tie: None
            }
        );
    }

    #[test]
    fn chord_after_bar() {
        assert_eq!(
            kinds("|[CEG]"),
            vec![
                bar("|"),
                MusicSymbol::Stem {
                    pitches: vec![pitch(Note::C), pitch(Note::E), pitch(Note::G)],
                    length: Rational32::from_integer(1),
                    decorations: vec![],
                    tie: None
                }
            ]
        );
    }

    #[test]
    fn grace_notes_1() {
        assert_eq!(
            kinds("{GdGe}A"),
            vec![
                MusicSymbol::GraceStart { acciaccatura: false },
                MusicSymbol::note(Note::G),
                n(Note::D, 2, 1),
                MusicSymbol::note(Note::G),
                n(Note::E, 2, 1),
                MusicSymbol::GraceEnd,
                MusicSymbol::note(Note::A),
            ]
        )
    }

    #[test]
    fn grace_notes_2() {
        assert_eq!(
            kinds("{/g}"),
            vec![
                MusicSymbol::GraceStart { acciaccatura: true },
                n(Note::G, 2, 1),
                MusicSymbol::GraceEnd,
            ]
        )
    }

    fn tuplet(p: i32, q: Option<i32>, r: Option<i32>) -> MusicSymbol {
        MusicSymbol::TupletStart { p, q, r }
    }

    #[test]
    fn tuplets() {
        let abc_notes = vec![
            MusicSymbol::note(Note::A),
            MusicSymbol::note(Note::B),
            MusicSymbol::note(Note::C),
        ];
        let with = |t: MusicSymbol| {
            let mut v = vec![t];
            v.extend(abc_notes.clone());
            v
        };
        assert_eq!(kinds("(3ABC"), with(tuplet(3, None, None)));
        assert_eq!(kinds("(3::ABC"), with(tuplet(3, None, None)));
        assert_eq!(kinds("(3:2ABC"), with(tuplet(3, Some(2), None)));
        assert_eq!(kinds("(3:2:3ABC"), with(tuplet(3, Some(2), Some(3))));
        assert_eq!(kinds("(3::2ABC"), with(tuplet(3, None, Some(2))));
        assert_eq!(
            kinds("(2AB"),
            vec![tuplet(2, None, None), MusicSymbol::note(Note::A), MusicSymbol::note(Note::B)]
        );
    }

    #[test]
    fn tuplet_with_spaces() {
        assert_eq!(
            kinds("(3 A B C"),
            vec![
                tuplet(3, None, None),
                MusicSymbol::VisualBreak,
                MusicSymbol::note(Note::A),
                MusicSymbol::VisualBreak,
                MusicSymbol::note(Note::B),
                MusicSymbol::VisualBreak,
                MusicSymbol::note(Note::C),
            ]
        );
    }

    #[test]
    fn tuplet_out_of_range() {
        assert_eq!(kinds("(1A")[0], MusicSymbol::SlurBegin);
        assert!(has_unknown("(1A"));
        assert!(has_unknown("(10ABC"));
    }

    #[test]
    fn default_tuplet_q() {
        assert_eq!(datatypes::default_tuplet_q(3, false), 2);
        assert_eq!(datatypes::default_tuplet_q(2, false), 3);
        assert_eq!(datatypes::default_tuplet_q(5, false), 2);
        assert_eq!(datatypes::default_tuplet_q(5, true), 3);
    }

    #[test]
    fn slurs() {
        assert_eq!(
            kinds("(AB)"),
            vec![
                MusicSymbol::SlurBegin,
                MusicSymbol::note(Note::A),
                MusicSymbol::note(Note::B),
                MusicSymbol::SlurEnd,
            ]
        );
    }

    fn broken(first_longer: bool, dots: u8) -> MusicSymbol {
        MusicSymbol::BrokenRhythm { first_longer, dots }
    }

    #[test]
    fn broken_rhythm() {
        let a = MusicSymbol::note(Note::A);
        let b = MusicSymbol::note(Note::B);
        assert_eq!(kinds("A>B"), vec![a.clone(), broken(true, 1), b.clone()]);
        assert_eq!(kinds("A<B"), vec![a.clone(), broken(false, 1), b.clone()]);
        assert_eq!(kinds("A>>B"), vec![a.clone(), broken(true, 2), b.clone()]);
        assert_eq!(
            kinds("A > B"),
            vec![
                a.clone(),
                MusicSymbol::VisualBreak,
                broken(true, 1),
                MusicSymbol::VisualBreak,
                b.clone()
            ]
        );
        // written lengths are kept; the broken rhythm is applied later
        assert_eq!(
            kinds("A2>B2"),
            vec![
                MusicSymbol::note_from_length(Note::A, 2),
                broken(true, 1),
                MusicSymbol::note_from_length(Note::B, 2)
            ]
        );
    }

    #[test]
    fn broken_rhythm_invalid() {
        assert!(has_unknown("A><B"));
        assert!(has_unknown("A>>>>B"));
    }

    #[test]
    fn chord_symbols_and_annotations() {
        assert_eq!(
            kinds("\"Am\"A\"^I\"B\"_low\"C"),
            vec![
                MusicSymbol::ChordSymbol("Am".to_string()),
                MusicSymbol::note(Note::A),
                MusicSymbol::Annotation { placement: Placement::Above, text: "I".to_string() },
                MusicSymbol::note(Note::B),
                MusicSymbol::Annotation { placement: Placement::Below, text: "low".to_string() },
                MusicSymbol::note(Note::C),
            ]
        );
        assert!(has_unknown("\"Am A B"));
    }

    #[test]
    fn inline_field() {
        let k = kinds("A|[K:G]B");
        assert_eq!(k.len(), 4);
        assert!(matches!(&k[2], MusicSymbol::InlineField(f) if f.name == 'K' && f.value == "G"));
    }

    #[test]
    fn standalone_decorations() {
        assert_eq!(
            kinds("!fermata!z"),
            vec![
                MusicSymbol::Decorations(vec![Decoration::Unresolved("fermata".to_string())]),
                MusicSymbol::Rest(Rest::Note(Rational32::from_integer(1)))
            ]
        );
        assert_eq!(
            kinds("Hz"),
            vec![
                MusicSymbol::Decorations(vec![Decoration::Fermata]),
                MusicSymbol::Rest(Rest::Note(Rational32::from_integer(1)))
            ]
        );
    }

    #[test]
    fn unknown_recovery() {
        assert_eq!(
            kinds("A y B"),
            vec![
                MusicSymbol::note(Note::A),
                MusicSymbol::VisualBreak,
                MusicSymbol::Unknown("y".to_string()),
                MusicSymbol::VisualBreak,
                MusicSymbol::note(Note::B),
            ]
        );
        assert_eq!(
            kinds("A??|B"),
            vec![
                MusicSymbol::note(Note::A),
                MusicSymbol::Unknown("??".to_string()),
                bar("|"),
                MusicSymbol::note(Note::B),
            ]
        );
    }

    #[test]
    fn overlay() {
        assert_eq!(
            kinds("A&B|"),
            vec![
                MusicSymbol::note(Note::A),
                MusicSymbol::Overlay,
                MusicSymbol::note(Note::B),
                bar("|"),
            ]
        );
        assert!(!has_unknown("AB & CD & EF | G"));
        let src = "X:1\nT:A\nK:G\nab & cd|\n";
        let d = parse(src);
        assert!(d.diagnostics.is_empty());
        let line = d.tunes[0].music_lines().next().unwrap();
        let amp = line.symbols.iter().find(|s| s.kind == MusicSymbol::Overlay).unwrap();
        assert_eq!(&src[amp.span.start..amp.span.end], "&");
    }

    //--------------------------------------------------------------------------
    // sharp notation

    #[test]
    fn raised_notes() {
        let raised = |note, octave| MusicSymbol::Stem {
            pitches: vec![Pitch::raised(note, octave)],
            length: Rational32::from_integer(1),
            decorations: vec![],
            tie: None,
        };
        assert_eq!(
            kinds("IKNQW"),
            vec![
                raised(Note::C, 1),
                raised(Note::D, 1),
                raised(Note::F, 1),
                raised(Note::G, 1),
                raised(Note::A, 1),
            ]
        );
        assert_eq!(kinds("i"), vec![raised(Note::C, 2)]);
        assert_eq!(kinds("W,"), vec![raised(Note::A, 0)]);
        assert_eq!(kinds("n'"), vec![raised(Note::F, 3)]);
    }

    #[test]
    fn raised_notes_with_length_tie_and_chord() {
        assert_eq!(
            abc::music_symbol("N2-").unwrap(),
            MusicSymbol::Stem {
                pitches: vec![Pitch::raised(Note::F, 1)],
                length: Rational32::from_integer(2),
                decorations: vec![],
                tie: Some(Tie::Solid),
            }
        );
        assert_eq!(
            abc::music_symbol("[DNA]").unwrap(),
            MusicSymbol::Stem {
                pitches: vec![pitch(Note::D), Pitch::raised(Note::F, 1), pitch(Note::A)],
                length: Rational32::from_integer(1),
                decorations: vec![],
                tie: None,
            }
        );
    }

    #[test]
    fn raised_notes_take_no_accidentals() {
        assert!(has_unknown("^I"));
        assert!(has_unknown("_k"));
    }

    #[test]
    fn raised_notes_keep_field_lines() {
        // K:, N:, Q:, W: and I: at the start of a line are still fields
        let d = parse("X:1\nT:A\nK:C\nIKN|\nQ:1/4=120\nW:words\n");
        let body = &d.tunes[0].body;
        assert!(matches!(&body[0], BodyLine::Music(m) if m.symbols.len() == 4));
        assert!(matches!(&body[1], BodyLine::Field(f) if f.name == 'Q'));
        assert!(matches!(&body[2], BodyLine::Field(f) if f.name == 'W'));
    }

    //--------------------------------------------------------------------------
    // single symbols

    #[test]
    fn note() {
        let s = abc::music_symbol("A").unwrap();
        assert_eq!(s, MusicSymbol::note(Note::A))
    }

    macro_rules! assert_note_len {
        ($input:tt, $expected:expr) => {
            assert_eq!(
                abc::music_symbol($input).unwrap(),
                MusicSymbol::note_from_length(Note::A, $expected)
            )
        };
    }

    #[test]
    fn note_length_1() {
        assert_note_len!("A3", Rational32::from_integer(3));
    }
    #[test]
    fn note_length_2() {
        assert_note_len!("A9001", Rational32::from_integer(9001));
    }
    #[test]
    fn note_length_3() {
        assert_note_len!("A/2", Rational32::new(1, 2));
    }
    #[test]
    fn note_length_4() {
        assert_note_len!("A/", Rational32::new(1, 2));
    }
    #[test]
    fn note_length_5() {
        assert_note_len!("A/4", Rational32::new(1, 4));
    }
    #[test]
    fn note_length_6() {
        assert_note_len!("A//", Rational32::new(1, 4));
    }
    #[test]
    fn note_length_7() {
        assert_note_len!("A3/2", Rational32::new(3, 2));
    }
    #[test]
    fn note_length_8() {
        assert_note_len!("A6821/962", Rational32::new(6821, 962));
    }
    #[test]
    fn note_length_9() {
        assert_note_len!("A3/", Rational32::new(3, 2));
    }
    #[test]
    fn note_length_10() {
        assert_note_len!("A3//", Rational32::new(3, 4));
    }

    #[test]
    fn note_length_invalid_1() {
        abc::music_symbol("A0").unwrap_err();
    }
    #[test]
    fn note_length_invalid_2() {
        abc::music_symbol("A-1").unwrap_err();
    }
    #[test]
    fn note_length_invalid_3() {
        abc::music_symbol("A99999999999").unwrap_err();
        abc::music_symbol("A/////////").unwrap_err();
    }

    #[test]
    fn letter_decorations_alone() {
        // decorations may stand before rests or bars, so these are valid alone
        assert_eq!(
            abc::music_symbol("H").unwrap(),
            MusicSymbol::Decorations(vec![Decoration::Fermata])
        );
        assert_eq!(
            abc::music_symbol("T").unwrap(),
            MusicSymbol::Decorations(vec![Decoration::Trill])
        );
    }

    #[test]
    fn note_invalid_1() {
        abc::music_symbol("h").unwrap_err();
    }
    #[test]
    fn note_invalid_2() {
        abc::music_symbol("Y").unwrap_err();
    }
    #[test]
    fn note_invalid_3() {
        abc::music_symbol("y").unwrap_err();
    }
    #[test]
    fn note_invalid_4() {
        abc::music_symbol("m").unwrap_err();
    }

    macro_rules! assert_bar {
        ($bar:tt) => {
            assert_eq!(
                abc::music_symbol($bar).unwrap(),
                MusicSymbol::Bar($bar.to_string())
            )
        };
    }

    #[test]
    fn bar_1() {
        assert_bar!("|")
    }
    #[test]
    fn bar_2() {
        assert_bar!("|]")
    }
    #[test]
    fn bar_3() {
        assert_bar!("||")
    }
    #[test]
    fn bar_4() {
        assert_bar!("[|")
    }
    #[test]
    fn bar_5() {
        assert_bar!("|:")
    }
    #[test]
    fn bar_6() {
        assert_bar!(":|")
    }
    #[test]
    fn bar_7() {
        assert_bar!("::")
    }
    #[test]
    fn bar_8() {
        assert_bar!("[|]")
    }

    macro_rules! assert_dec {
        ($input:tt, $expected:expr) => {
            assert_eq!(
                abc::music_symbol($input).unwrap(),
                MusicSymbol::new_note($expected, None, Note::A, 1, Rational32::from_integer(1), None)
            )
        };
    }

    #[test]
    fn decoration_1() {
        assert_dec!(".A", vec![Decoration::Staccato])
    }
    #[test]
    fn decoration_2() {
        assert_dec!("~A", vec![Decoration::Roll])
    }
    #[test]
    fn decoration_3() {
        assert_dec!("HA", vec![Decoration::Fermata])
    }
    #[test]
    fn decoration_4() {
        assert_dec!("LA", vec![Decoration::Accent])
    }
    #[test]
    fn decoration_5() {
        assert_dec!("MA", vec![Decoration::LowerMordent])
    }
    #[test]
    fn decoration_6() {
        assert_dec!("OA", vec![Decoration::Coda])
    }
    #[test]
    fn decoration_7() {
        assert_dec!("PA", vec![Decoration::UpperMordent])
    }
    #[test]
    fn decoration_8() {
        assert_dec!("SA", vec![Decoration::Segno])
    }
    #[test]
    fn decoration_9() {
        assert_dec!("TA", vec![Decoration::Trill])
    }
    #[test]
    fn decoration_10() {
        assert_dec!("uA", vec![Decoration::UpBow])
    }
    #[test]
    fn decoration_11() {
        assert_dec!("vA", vec![Decoration::DownBow])
    }
    #[test]
    fn decoration_12() {
        assert_dec!(
            "!somedec!A",
            vec![Decoration::Unresolved("somedec".to_string())]
        )
    }
    #[test]
    fn decoration_13() {
        assert_dec!("+trill+A", vec![Decoration::Unresolved("trill".to_string())])
    }

    #[test]
    fn multiple_decorations() {
        assert_dec!("v.A", vec![Decoration::DownBow, Decoration::Staccato]);
    }

    #[test]
    fn decoration_invalid() {
        abc::music_symbol("!A").unwrap_err();
    }

    macro_rules! assert_acc {
        ($input:tt, $expected:expr) => {
            assert_eq!(
                abc::music_symbol($input).unwrap(),
                MusicSymbol::new_note(vec![], Some($expected), Note::A, 1, Rational32::from_integer(1), None)
            )
        };
    }

    #[test]
    fn accidental_1() {
        assert_acc!("^A", Accidental::Sharp)
    }
    #[test]
    fn accidental_2() {
        assert_acc!("_A", Accidental::Flat)
    }
    #[test]
    fn accidental_3() {
        assert_acc!("=A", Accidental::Natural)
    }
    #[test]
    fn accidental_4() {
        assert_acc!("^^A", Accidental::DoubleSharp)
    }
    #[test]
    fn accidental_5() {
        assert_acc!("__A", Accidental::DoubleFlat)
    }

    #[test]
    fn accidental_invalid_1() {
        abc::music_symbol("^^^A").unwrap_err();
    }
    #[test]
    fn accidental_invalid_2() {
        abc::music_symbol("___A").unwrap_err();
    }
    #[test]
    fn accidental_invalid_3() {
        abc::music_symbol("_^_A").unwrap_err();
    }
    #[test]
    fn accidental_invalid_4() {
        abc::music_symbol("^_A").unwrap_err();
    }

    macro_rules! assert_oct {
        ($input:tt, $expected:expr) => {
            assert_eq!(
                abc::music_symbol($input).unwrap(),
                MusicSymbol::new_note(vec![], None, Note::A, $expected, Rational32::from_integer(1), None)
            )
        };
    }

    #[test]
    fn octave_1() {
        assert_oct!("A,", 0)
    }
    #[test]
    fn octave_2() {
        assert_oct!("A'", 2)
    }
    #[test]
    fn octave_3() {
        assert_oct!("A,,,", -2)
    }
    #[test]
    fn octave_4() {
        assert_oct!("A''''", 5)
    }
    #[test]
    fn octave_5() {
        assert_oct!("A,'", 1)
    }
    #[test]
    fn octave_6() {
        assert_oct!("A,'',,','", 1)
    }

    #[test]
    fn diatonic() {
        assert_eq!(Pitch::new(Note::C, 1, None, None).diatonic(), 0);
        assert_eq!(Pitch::new(Note::E, 1, None, None).diatonic(), 2);
        assert_eq!(Pitch::new(Note::C, 2, None, None).diatonic(), 7);
        assert_eq!(Pitch::new(Note::B, 0, None, None).diatonic(), -1);
    }

    macro_rules! assert_rst {
        ($input:tt, $expected:expr) => {
            assert_eq!(
                abc::music_symbol($input).unwrap(),
                MusicSymbol::Rest($expected)
            )
        };
    }

    #[test]
    fn rest_1() {
        assert_rst!("z", Rest::Note(Rational32::from_integer(1)))
    }
    #[test]
    fn rest_2() {
        assert_rst!("x", Rest::NoteHidden(Rational32::from_integer(1)))
    }
    #[test]
    fn rest_3() {
        assert_rst!("Z", Rest::Measure(1))
    }
    #[test]
    fn rest_4() {
        assert_rst!("X", Rest::MeasureHidden(1))
    }
    #[test]
    fn rest_5() {
        assert_rst!("z3", Rest::Note(Rational32::from_integer(3)))
    }
    #[test]
    fn rest_6() {
        assert_rst!("Z10", Rest::Measure(10))
    }
    #[test]
    fn rest_7() {
        assert_rst!("x7", Rest::NoteHidden(Rational32::from_integer(7)))
    }
    #[test]
    fn rest_8() {
        assert_rst!("X900", Rest::MeasureHidden(900))
    }
}
