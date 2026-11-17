//! `%%score`: which voices are printed, in which order, and how their staves
//! are joined.
//!
//! ```text
//! %%score [ (S A) | (T B) ] { RH LH }
//! ```
//!
//! - a voice id is a voice on a staff of its own
//! - `( )` voices that share a staff
//! - `[ ]` staves joined by a bracket
//! - `{ }` staves joined by a brace
//! - `|` bar lines run through from the staff before it to the staff after it
//!
//! Voices that are not named are not printed. `%%staves` is the older form of
//! the directive: there, bar lines run through all the staves of a bracket or
//! brace, and `|` stops them. `I:score` and `I:staves` in the header mean the
//! same as the directives.

use crate::voices::Voice;
use abc::datatypes::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GroupKind {
    Brace,
    Bracket,
}

/// Staves, or voices once they are arranged, joined by a brace or a bracket.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Group {
    pub kind: GroupKind,
    pub first: usize,
    pub last: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StaffSpec {
    /// The ids of the voices on the staff.
    pub voices: Vec<String>,
    /// Bar lines run through to the next staff.
    pub joined: bool,
}

/// What a `%%score` directive says.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ScoreLayout {
    pub staves: Vec<StaffSpec>,
    pub groups: Vec<Group>,
}

/// Reads the value of a `%%score` directive, or of `%%staves` when
/// `staves_rule` is set. A voice that is named twice counts once.
pub fn parse_score(value: &str, staves_rule: bool) -> ScoreLayout {
    let mut layout = ScoreLayout::default();
    // the groups that are open, each with its first staff
    let mut open: Vec<(GroupKind, usize)> = vec![];
    let mut in_parentheses = false;
    // the next voice starts a staff
    let mut new_staff = true;
    // a `|` since the last staff
    let mut bar = false;
    let mut id = String::new();

    let close = |layout: &mut ScoreLayout, open: &mut Vec<(GroupKind, usize)>, kind: GroupKind| {
        let Some(i) = open.iter().rposition(|(k, _)| *k == kind) else {
            return;
        };
        let (_, first) = open.remove(i);
        let count = layout.staves.len() - first.min(layout.staves.len());
        // a brace joins at least two staves
        if count > 1 || (count == 1 && kind == GroupKind::Bracket) {
            layout.groups.push(Group {
                kind,
                first,
                last: layout.staves.len() - 1,
            });
        }
    };

    // a comment ends the directive
    let text = value.split('%').next().unwrap_or("");
    for c in text.chars().chain(std::iter::once(' ')) {
        if !c.is_whitespace() && !"(){}[]|*".contains(c) {
            id.push(c);
            continue;
        }
        if !id.is_empty() {
            let id = std::mem::take(&mut id);
            if !layout.staves.iter().any(|s| s.voices.contains(&id)) {
                if new_staff {
                    let count = layout.staves.len();
                    if let Some(before) = layout.staves.last_mut() {
                        // the staff before is inside a bracket or brace that is still open
                        let grouped = open.iter().any(|(_, first)| *first < count);
                        before.joined = if staves_rule { grouped && !bar } else { bar };
                    }
                    layout.staves.push(StaffSpec::default());
                    bar = false;
                }
                if let Some(staff) = layout.staves.last_mut() {
                    staff.voices.push(id);
                }
                new_staff = !in_parentheses;
            }
        }
        match c {
            '(' => {
                in_parentheses = true;
                new_staff = true;
            }
            ')' => {
                in_parentheses = false;
                new_staff = true;
            }
            '{' => open.push((GroupKind::Brace, layout.staves.len())),
            '[' => open.push((GroupKind::Bracket, layout.staves.len())),
            '}' => close(&mut layout, &mut open, GroupKind::Brace),
            ']' => close(&mut layout, &mut open, GroupKind::Bracket),
            '|' => bar = true,
            // `*` marks a floating voice, which is printed like any other
            _ => {}
        }
    }
    // groups that were never closed end with the last staff
    while let Some((kind, _)) = open.last().copied() {
        close(&mut layout, &mut open, kind);
    }
    layout
}

/// The layout a tune asks for, with the text that asks for it: the last
/// `%%score` or `%%staves` of the header, or else its last `I:score` or
/// `I:staves`.
pub fn score_layout(tune: &Tune) -> Option<(ScoreLayout, Span)> {
    let directive = tune
        .directives
        .iter()
        .rev()
        .find(|d| d.name == "score" || d.name == "staves")
        .map(|d| (d.name.as_str(), d.value.as_str(), d.span));
    let field = || {
        tune.header.iter().rev().filter(|f| f.name == 'I').find_map(|f| {
            let (name, value) = f.value.split_once(char::is_whitespace)?;
            (name == "score" || name == "staves").then_some((name, value, f.span))
        })
    };
    let (name, value, span) = directive.or_else(field)?;
    let layout = parse_score(value, name == "staves");
    (!layout.staves.is_empty()).then_some((layout, span))
}

/// The voices of a tune as they are printed, top to bottom.
pub struct Arrangement<'a> {
    pub voices: Vec<Voice<'a>>,
    /// For each voice, the staff it is on, counted from the top. Voices that
    /// share a staff follow each other.
    pub staff_of: Vec<usize>,
    /// For each staff: bar lines run through to the next one.
    pub joined: Vec<bool>,
    /// Braces and brackets, from their first to their last staff.
    pub groups: Vec<Group>,
}

/// Puts the voices of a tune on staves in the order its `%%score` asks for
/// and leaves out the voices it does not name. Without a `%%score` every
/// voice is printed on a staff of its own, in the order the voices appear.
/// Voices that the directive names but the tune does not have are reported.
pub fn arrange<'a>(tune: &Tune, voices: Vec<Voice<'a>>, diagnostics: &mut Vec<Diagnostic>) -> Arrangement<'a> {
    let layout = score_layout(tune).filter(|_| !voices.is_empty());
    let Some((layout, span)) = layout else {
        let count = voices.len();
        return Arrangement {
            voices,
            staff_of: (0..count).collect(),
            joined: vec![false; count],
            groups: vec![],
        };
    };

    let mut pool: Vec<Option<Voice>> = voices.into_iter().map(Some).collect();
    let mut printed = vec![];
    // the staff of the layout that each printed voice is on
    let mut wanted = vec![];
    let mut missing = vec![];
    for (s, staff) in layout.staves.iter().enumerate() {
        for id in &staff.voices {
            match pool.iter_mut().find(|v| v.as_ref().is_some_and(|v| &v.id == id)) {
                Some(voice) => {
                    printed.extend(voice.take());
                    wanted.push(s);
                }
                None => missing.push(id.as_str()),
            }
        }
    }
    if !missing.is_empty() {
        diagnostics.push(Diagnostic::new(
            span,
            format!("the tune has no voice {}", missing.join(", ")),
        ));
    }

    // the staves of the layout that have a voice, top to bottom
    let mut kept = wanted.clone();
    kept.dedup();
    let staff_of = wanted
        .iter()
        .map(|s| kept.iter().position(|k| k == s).unwrap_or(0))
        .collect();
    // bar lines run through when every staff break between two printed staves is joined
    let joined = (0..kept.len())
        .map(|i| match kept.get(i + 1) {
            Some(&next) => layout.staves[kept[i]..next].iter().all(|s| s.joined),
            None => false,
        })
        .collect();
    let groups = layout
        .groups
        .iter()
        .filter_map(|g| {
            let inside = |s: &usize| (g.first..=g.last).contains(s);
            let first = kept.iter().position(inside)?;
            let last = kept.iter().rposition(inside)?;
            (last > first || g.kind == GroupKind::Bracket).then_some(Group {
                kind: g.kind,
                first,
                last,
            })
        })
        .collect();
    Arrangement {
        voices: printed,
        staff_of,
        joined,
        groups,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voices::voices;

    /// The layout as text: staves as their voices joined by `+`, with `|`
    /// after a staff whose bar lines run through, then the groups.
    fn shown(layout: &ScoreLayout) -> String {
        let mut out: Vec<String> = layout
            .staves
            .iter()
            .map(|s| format!("{}{}", s.voices.join("+"), if s.joined { "|" } else { "" }))
            .collect();
        for g in &layout.groups {
            let (a, b) = if g.kind == GroupKind::Brace { ('{', '}') } else { ('[', ']') };
            out.push(format!("{}{}-{}{}", a, g.first, g.last, b));
        }
        out.join(" ")
    }

    fn score(value: &str) -> String {
        shown(&parse_score(value, false))
    }

    fn staves(value: &str) -> String {
        shown(&parse_score(value, true))
    }

    #[test]
    fn voices_and_staves() {
        assert_eq!(score("1 2 3"), "1 2 3");
        assert_eq!(score("T1 T2 B1 B2"), "T1 T2 B1 B2");
        assert_eq!(score("(T1 T2) (B1 B2)"), "T1+T2 B1+B2");
        assert_eq!(score("(1 2)3(4 5 6)"), "1+2 3 4+5+6");
        assert_eq!(score("  1   (2 3)  % the usual"), "1 2+3");
        assert_eq!(score(""), "");
        // a voice is printed once
        assert_eq!(score("1 2 1 (3 2)"), "1 2 3");
    }

    #[test]
    fn braces_and_brackets() {
        assert_eq!(score("{1 2}"), "1 2 {0-1}");
        assert_eq!(score("[1 2 3]"), "1 2 3 [0-2]");
        assert_eq!(score("[S A T B] {RH LH}"), "S A T B RH LH [0-3] {4-5}");
        assert_eq!(score("[(S A) (T B)] {(RH1 RH2) LH}"), "S+A T+B RH1+RH2 LH [0-1] {2-3}");
        // one inside the other
        assert_eq!(score("[1 {2 3} 4]"), "1 2 3 4 {1-2} [0-3]");
        // a bracket may hold one staff, a brace needs two
        assert_eq!(score("[1] {2}"), "1 2 [0-0]");
        // what is left open ends with the last staff
        assert_eq!(score("1 [2 3"), "1 2 3 [1-2]");
        assert_eq!(score("1 2] }"), "1 2");
    }

    #[test]
    fn bar_lines_in_score() {
        // only where a bar is written
        assert_eq!(score("1 | 2 3"), "1| 2 3");
        assert_eq!(score("[1 2 3]"), "1 2 3 [0-2]");
        assert_eq!(score("{1 | 2} 3 | 4"), "1| 2 3| 4 {0-1}");
        assert_eq!(score("[(S A) | (T B)]"), "S+A| T+B [0-1]");
    }

    #[test]
    fn bar_lines_in_staves() {
        // through a bracket or brace unless a bar is written, and nowhere else
        assert_eq!(staves("[1 2 3]"), "1| 2| 3 [0-2]");
        assert_eq!(staves("[1 2 | 3] 4 5"), "1| 2 3 4 5 [0-2]");
        assert_eq!(staves("1 2 {3 4}"), "1 2 3| 4 {2-3}");
        assert_eq!(staves("{1 2} {3 4}"), "1| 2 3| 4 {0-1} {2-3}");
        assert_eq!(staves("[1 {2 3} 4]"), "1| 2| 3| 4 {1-2} [0-3]");
        assert_eq!(staves("[1 {2 | 3}] 4"), "1| 2 3 4 {1-2} [0-2]");
    }

    fn layout_of(header: &str) -> Option<String> {
        let doc = abc::parse(&format!("X:1\nT:T\n{}K:C\nabc\n", header));
        score_layout(&doc.tunes[0]).map(|(l, _)| shown(&l))
    }

    #[test]
    fn the_layout_of_a_tune() {
        assert_eq!(layout_of(""), None);
        assert_eq!(layout_of("%%score (1 2) 3\n").as_deref(), Some("1+2 3"));
        assert_eq!(layout_of("%%staves [1 2]\n").as_deref(), Some("1| 2 [0-1]"));
        assert_eq!(layout_of("I:score 2 1\n").as_deref(), Some("2 1"));
        // the last directive wins, and a directive wins over a field
        assert_eq!(layout_of("%%score 1 2\n%%staves [2 1]\n").as_deref(), Some("2| 1 [0-1]"));
        assert_eq!(layout_of("%%score 1 2\nI:score 2 1\n").as_deref(), Some("1 2"));
        // nothing named is no layout
        assert_eq!(layout_of("%%score\n"), None);
        assert_eq!(layout_of("%%score % later\n"), None);
        assert_eq!(layout_of("I:linebreak $\n"), None);
    }

    /// The printed staves as their voices joined by `+`, with `|` after a
    /// staff whose bar lines run through, then the groups, then the number of
    /// diagnostics.
    fn arranged(src: &str) -> String {
        let doc = abc::parse(src);
        let mut diagnostics = vec![];
        let a = arrange(&doc.tunes[0], voices(&doc.tunes[0]), &mut diagnostics);
        let mut out: Vec<String> = a
            .joined
            .iter()
            .enumerate()
            .map(|(s, joined)| {
                let ids: Vec<&str> = a
                    .voices
                    .iter()
                    .zip(&a.staff_of)
                    .filter(|(_, staff)| **staff == s)
                    .map(|(v, _)| v.id.as_str())
                    .collect();
                format!("{}{}", ids.join("+"), if *joined { "|" } else { "" })
            })
            .collect();
        for g in &a.groups {
            let (o, c) = if g.kind == GroupKind::Brace { ('{', '}') } else { ('[', ']') };
            out.push(format!("{}{}-{}{}", o, g.first, g.last, c));
        }
        out.push(format!("{}", diagnostics.len()));
        out.join(" ")
    }

    const FOUR: &str = "K:C\n[V:S]abc\n[V:A]abc\n[V:T]abc\n[V:B]abc\n";

    #[test]
    fn voices_follow_the_score() {
        let tune = |header: &str| arranged(&format!("X:1\nT:T\n{}{}", header, FOUR));
        // without a layout: as they appear, a staff each
        assert_eq!(tune(""), "S A T B 0");
        assert_eq!(tune("%%score B T A S\n"), "B T A S 0");
        // voices that are not named are not printed
        assert_eq!(tune("%%score T S\n"), "T S 0");
        // voices in parentheses share a staff
        assert_eq!(tune("%%score (S A) (T B)\n"), "S+A T+B 0");
        assert_eq!(tune("%%score (S A T B)\n"), "S+A+T+B 0");
        assert_eq!(tune("%%score S (T A) B\n"), "S T+A B 0");
    }

    #[test]
    fn groups_and_bar_lines_follow_the_staves() {
        let tune = |header: &str| arranged(&format!("X:1\nT:T\n{}{}", header, FOUR));
        assert_eq!(tune("%%score [S A T B]\n"), "S A T B [0-3] 0");
        assert_eq!(tune("%%score [S | A | T | B]\n"), "S| A| T| B [0-3] 0");
        assert_eq!(tune("%%staves [S A] {T B}\n"), "S| A T| B [0-1] {2-3} 0");
        assert_eq!(tune("%%score [(S A) | (T B)]\n"), "S+A| T+B [0-1] 0");
        // a group shrinks to the staves that are there
        assert_eq!(tune("%%score [X S A] {T Y}\n"), "S A T [0-1] 1");
        // a missing staff does not break the bar lines around it
        assert_eq!(tune("%%score S | X | A\n"), "S| A 1");
        // and a staff keeps the voices it has
        assert_eq!(tune("%%score (S X) | (Y A T)\n"), "S| A+T 1");
    }

    #[test]
    fn missing_voices_are_reported() {
        let src = format!("X:1\nT:T\n%%score S X A Y\n{}", FOUR);
        let doc = abc::parse(&src);
        let mut diagnostics = vec![];
        arrange(&doc.tunes[0], voices(&doc.tunes[0]), &mut diagnostics);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].message, "the tune has no voice X, Y");
        assert_eq!(&src[diagnostics[0].span.start..diagnostics[0].span.end], "%%score S X A Y");
        // a tune without music has nothing to report
        let doc = abc::parse("X:1\nT:T\n%%score S X\nK:C\n");
        let mut diagnostics = vec![];
        let a = arrange(&doc.tunes[0], voices(&doc.tunes[0]), &mut diagnostics);
        assert!(a.voices.is_empty() && diagnostics.is_empty());
    }
}
