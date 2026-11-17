//! Turns tunes into systems of positioned items.
//!
//! Each music line (joined with the next one when it ends in `\`) becomes one
//! staff of a system. The first lines of all the voices of a tune form its
//! first system, their second lines the second, and so on. A system is built
//! in four passes: each staff reads its symbols into columns, then works out
//! note values, beams and stem directions; the system places the columns of
//! all its staves horizontally; then each staff draws.
//!
//! Voices that share a staff (`%%score (1 2)`) are each read and drawn like a
//! staff of their own, at the same place: only the first draws the lines and
//! the clef, and their stems point away from each other. The music after an
//! `&` is such a voice too, for the length of its bar.
//!
//! Columns are placed by the time they start at. Columns of different staves
//! that start at the same time form a moment and share an x position.

use crate::glyphs as g;
use crate::lyrics::{Sung, Syllable, Underlay, align};
use crate::music::*;
use crate::score_layout::{Arrangement, GroupKind, arrange};
use crate::voices::{Event, Voice, VoiceProps, voices};
use crate::{Anchor, Engraving, Hit, Item, Options, Rect, TextStyle};
use abc::datatypes::*;
use num_rational::{Rational32, Rational64};

const HEAD_W: f32 = 1.18;
const GRACE: f32 = 0.6;
const CHORD_SIZE: f32 = 1.6;
const SMALL_TEXT: f32 = 1.4;
/// Size of the voice names before the staves.
const LABEL_SIZE: f32 = 1.5;
/// The least distance from the bottom line of a staff to the top line of the
/// next staff of its system.
const STAFF_GAP: f32 = 5.0;
/// Extra room below a system of several staves.
const SYSTEM_GAP: f32 = 3.0;
const LYRIC_SIZE: f32 = 1.6;
/// From the lowest mark of a staff to the top of its first verse.
const LYRIC_GAP: f32 = 0.7;
/// From one verse to the next, in text sizes.
const LYRIC_LEADING: f32 = 1.25;
/// The least room between two syllables.
const LYRIC_PAD: f32 = 0.55;
/// Thickness of the line after a held syllable.
const LYRIC_LINE: f32 = 0.1;
/// Size of the words printed after the tune (`W:`).
const WORDS_SIZE: f32 = 1.6;
/// From the staves to the thick line of a bracket, and how thick that line is.
const BRACKET_GAP: f32 = 0.45;
const BRACKET_LINE: f32 = 0.5;
/// From the staves, or from a bracket, to a brace, and the most a brace is wide.
const BRACE_GAP: f32 = 0.35;
const BRACE_WIDTH: f32 = 1.7;

pub fn engrave(doc: &Document, opt: &Options) -> Engraving {
    let mut page = Engraving {
        width: opt.width,
        height: 0.0,
        items: vec![],
        hits: vec![],
        diagnostics: vec![],
        text_font: opt.text_font.clone(),
        music_font: opt.music_font.clone(),
    };
    let mut y = opt.margin;
    for (i, tune) in doc.tunes.iter().enumerate() {
        if i > 0 {
            y += 5.0;
        }
        y = engrave_tune(doc, tune, opt, y, &mut page);
    }
    page.height = y + opt.margin;
    page
}

//------------------------------------------------------------------------------
// tunes

#[derive(Clone, Debug)]
struct State {
    /// Sharp notation: the tune writes black keys as I K N Q W and has no key signature.
    sharp_notation: bool,
    fifths: i32,
    clef: Clef,
    meter: Meter,
    unit: Rational32,
    /// Diatonic steps added to every written pitch (`middle=` and `octave=`).
    shift: i32,
    /// Stems forced up (`true`) or down by `stem=`.
    stem: Option<bool>,
}

impl State {
    /// A clef written in the music. `middle=` belonged to the old clef.
    fn set_clef(&mut self, clef: Clef) {
        self.clef = clef;
        self.shift = 0;
    }
}

/// What continues from one system to the next.
#[derive(Default)]
struct Carry {
    /// Ties still open at the end of the previous system: (diatonic pitch, curves upwards).
    ties: Vec<(i32, bool)>,
    /// Slurs still open at the end of the previous system.
    slurs: usize,
}

fn engrave_tune(doc: &Document, tune: &Tune, opt: &Options, mut y: f32, page: &mut Engraving) -> f32 {
    let header: Vec<&InfoField> = doc.file_header.iter().chain(tune.header.iter()).collect();
    let last = |name: char| header.iter().rev().find(|f| f.name == name).copied();

    let meter = last('M').map(|f| parse_meter(&f.value)).unwrap_or(Meter::None);
    let unit = last('L')
        .and_then(|f| parse_length(&f.value))
        .unwrap_or_else(|| default_unit(&meter));
    let key = last('K').map(|f| parse_key(&f.value)).unwrap_or(Key {
        fifths: 0,
        clef: None,
    });
    let sharp_notation = uses_sharp_notation(tune);
    let state = State {
        sharp_notation,
        fifths: if sharp_notation { 0 } else { key.fifths },
        clef: key.clef.unwrap_or(Clef::Treble),
        meter,
        unit,
        shift: 0,
        stem: None,
    };

    // title block
    let text = |page: &mut Engraving, x: f32, y: f32, t: &str, size: f32, anchor: Anchor, span: Span| {
        let item = Item::Text {
            x,
            y,
            text: t.to_string(),
            size,
            anchor,
            style: TextStyle::Normal,
        };
        page.hits.push(Hit {
            rect: item.bounds(),
            span,
        });
        page.items.push(item);
    };
    for (i, f) in tune.header.iter().filter(|f| f.name == 'T').enumerate() {
        let size = if i == 0 { 2.6 } else { 2.0 };
        y += size;
        text(page, opt.width / 2.0, y, &f.value, size, Anchor::Middle, f.span);
        y += 0.6;
    }
    let composer = last('C');
    let rhythm = last('R');
    if composer.is_some() || rhythm.is_some() {
        y += 1.6;
        if let Some(c) = composer {
            text(page, opt.width - opt.margin, y, &c.value, 1.5, Anchor::End, c.span);
        }
        if let Some(r) = rhythm {
            text(page, opt.margin, y, &r.value, 1.5, Anchor::Start, r.span);
        }
        y += 0.4;
    }
    y += 1.0;

    // the voices that are printed, top to bottom, the staff each is on, and
    // what joins the staves
    let Arrangement {
        voices: printed,
        staff_of,
        joined,
        groups,
    } = arrange(tune, voices(tune), &mut page.diagnostics);
    let staff_count = joined.len();
    // a bracket stands next to the staves, a brace to the left of any bracket
    let has = |kind: GroupKind| groups.iter().any(|g| g.kind == kind);
    let bracket_room = if has(GroupKind::Bracket) { BRACKET_GAP + BRACKET_LINE + 0.25 } else { 0.0 };
    let brace_room = if has(GroupKind::Brace) { BRACE_GAP + BRACE_WIDTH + 0.25 } else { 0.0 };
    let group_room = bracket_room + brace_room;

    // voices that share a staff take the clef of its first voice, and their
    // stems go up and down in turn unless `stem=` says otherwise
    let mut parts: Vec<Part> = vec![];
    for (voice, &staff) in printed.into_iter().zip(&staff_of) {
        let before = parts.iter().filter(|p| p.staff == staff).count();
        let shared = staff_of.iter().filter(|s| **s == staff).count() > 1;
        let clef = parts.iter().find(|p| p.staff == staff).map(|p| p.state.clef).or(voice.props.clef);
        parts.push(Part {
            state: voice_state(&state, &voice.props, clef),
            underlay: align(&voice.lyrics, &mut page.diagnostics),
            role: shared.then(|| voice.props.stem.unwrap_or(before % 2 == 0)),
            voice,
            staff,
            next: 0,
            carry: Carry::default(),
            show_meter: true,
        });
    }

    // systems: system k has line k of every voice
    let systems = parts.iter().map(|p| p.voice.lines()).max().unwrap_or(0);
    let inner = opt.width - 2.0 * opt.margin;
    for k in 0..systems {
        // voice names: the full name before the first system, the short one after
        let labels: Vec<Option<String>> = parts
            .iter()
            .map(|p| {
                let props = &p.voice.props;
                if k == 0 { props.name.clone() } else { props.subname.clone() }
            })
            .collect();
        let indent = labels
            .iter()
            .flatten()
            .map(|l| crate::text_width(l, LABEL_SIZE) + 1.2)
            .fold(0.0, f32::max)
            + group_room;

        let mut sys = System::new(inner - indent);
        // the source text and the length of each voice's line, if it has one
        let mut lines: Vec<Option<(Span, Rational64)>> = vec![];
        // for each staff of the system, the voice whose ties and slurs it carries on
        let mut carries: Vec<Option<usize>> = vec![];
        for (i, p) in parts.iter_mut().enumerate() {
            // the fields written before the voice's next line come first
            let mut symbols: Option<&[&Symbol]> = None;
            while let Some(event) = p.voice.events.get(p.next) {
                p.next += 1;
                match event {
                    Event::Field(f) => apply_field(&mut p.state, &mut p.show_meter, f),
                    Event::Line(line) => {
                        symbols = Some(line);
                        break;
                    }
                }
            }
            let mut staff = Staff::new(p.state.clone());
            staff.place = p.staff;
            staff.lead = !sys.staves.iter().any(|s| s.place == p.staff);
            staff.role = p.role;
            let mut overlays = vec![];
            if let Some(symbols) = symbols {
                // the music after each `&` is a voice of its own
                let (music, extra) = split_overlays(symbols);
                let start = p.state.clone();
                staff.read(&music, &mut p.state, &mut p.carry);
                staff.set_lyrics(&p.underlay);
                overlays = overlay_staves(&mut staff, &extra, &start, &mut page.diagnostics);
            }
            staff.prepare(p.show_meter);
            p.show_meter = false;
            lines.push(symbols.map(|s| (line_span(s), staff.duration())));
            carries.push(symbols.is_some().then_some(i));
            sys.staves.push(staff);
            for mut overlay in overlays {
                overlay.prepare(false);
                carries.push(None);
                sys.staves.push(overlay);
            }
        }
        // repeat endings are drawn once for each staff, by the first of its voices that has them
        let mut bracketed = vec![false; staff_count];
        for staff in sys.staves.iter_mut().filter(|s| !s.endings.is_empty()) {
            if std::mem::replace(&mut bracketed[staff.place], true) {
                staff.endings.clear();
            }
        }
        sys.space();
        for (staff, voice) in sys.staves.iter_mut().zip(&carries) {
            match voice {
                Some(i) => staff.draw(&mut parts[*i].carry),
                // a staff without music leaves open ties and slurs to the voice's
                // next line, and the music after an `&` ends with its bar
                None => staff.draw(&mut Carry::default()),
            }
        }
        // lyrics go below everything on their staff, one voice's verses after the other's
        for place in 0..staff_count {
            let low = sys
                .staves
                .iter()
                .filter(|s| s.place == place)
                .map(|s| s.low())
                .fold(4.0_f32, f32::max);
            let mut row = 0;
            for staff in sys.staves.iter_mut().filter(|s| s.place == place) {
                staff.draw_lyrics(low, row);
                row += staff.verses();
            }
        }

        // lines that sound together should be equally long; most voices are right
        let lengths: Vec<Rational64> = lines.iter().flatten().map(|l| l.1).collect();
        let usual = lengths
            .iter()
            .copied()
            .max_by_key(|d| (lengths.iter().filter(|o| *o == d).count(), *d));
        for (p, line) in parts.iter().zip(&lines) {
            if let (Some((span, length)), Some(usual)) = (line, usual)
                && *length != usual
            {
                let unit = wide(p.state.unit);
                page.diagnostics.push(Diagnostic::new(
                    *span,
                    format!(
                        "voice {}: this line is {} units of L:{} long, the other voices have {}",
                        p.voice.id,
                        length / unit,
                        p.state.unit,
                        usual / unit
                    ),
                ));
            }
        }

        // the staves, down the page
        let x0 = opt.margin + indent;
        let end_bar = sys.end_bar();
        // for each staff, the bar lines that run through to the staff below
        let through: Vec<Vec<(f32, f32)>> = (0..staff_count)
            .map(|s| if joined[s] && s + 1 < staff_count { sys.shared_bars(s) } else { vec![] })
            .collect();
        let mut tops: Vec<f32> = vec![];
        for place in 0..staff_count {
            // everything on the staff, whichever of its voices drew it
            let (min_y, max_y) = sys
                .staves
                .iter()
                .filter(|s| s.place == place)
                .map(|s| s.extent())
                .fold((f32::MAX, f32::MIN), |a, e| (a.0.min(e.0), a.1.max(e.1)));
            let mut top = y - min_y;
            if let Some(above) = tops.last() {
                top = top.max(above + 4.0 + STAFF_GAP);
            }
            tops.push(top);
            y = top + max_y + if place + 1 < staff_count { 1.0 } else { 2.0 };
        }
        for staff in sys.staves {
            let top = tops[staff.place];
            for mut item in staff.items {
                item.translate(x0, top);
                page.items.push(item);
            }
            for h in staff.hits {
                page.hits.push(Hit {
                    rect: h.rect.translate(x0, top),
                    span: h.span,
                });
            }
        }
        if staff_count > 1 {
            // a line joins the staves at the left
            let x = x0 + g::THIN_BARLINE / 2.0;
            page.items.push(Item::Line {
                x1: x,
                y1: tops[0] - g::STAFF_LINE / 2.0,
                x2: x,
                y2: tops[staff_count - 1] + 4.0 + g::STAFF_LINE / 2.0,
                width: g::THIN_BARLINE,
            });
            // and the bar line that ends the system closes it on the right
            for (x, width) in end_bar {
                page.items.push(Item::Line {
                    x1: x0 + x,
                    y1: tops[0],
                    x2: x0 + x,
                    y2: tops[staff_count - 1] + 4.0,
                    width,
                });
            }
            y += SYSTEM_GAP;
        }
        for (i, lines) in through.iter().enumerate() {
            for (x, width) in lines {
                page.items.push(Item::Line {
                    x1: x0 + x,
                    y1: tops[i] + 4.0,
                    x2: x0 + x,
                    y2: tops[i + 1],
                    width: *width,
                });
            }
        }
        for group in &groups {
            let (top, bottom) = (tops[group.first], tops[group.last] + 4.0);
            match group.kind {
                GroupKind::Bracket => {
                    // a thick line with a curved end above and below the staves
                    let x = x0 - BRACKET_GAP - BRACKET_LINE;
                    page.items.push(Item::Line {
                        x1: x + BRACKET_LINE / 2.0,
                        y1: top,
                        x2: x + BRACKET_LINE / 2.0,
                        y2: bottom,
                        width: BRACKET_LINE,
                    });
                    for (y, glyph) in [(top, g::BRACKET_TOP), (bottom, g::BRACKET_BOTTOM)] {
                        page.items.push(Item::Glyph {
                            x,
                            y,
                            glyph,
                            scale: 1.0,
                        });
                    }
                }
                GroupKind::Brace => page.items.extend(brace(x0 - bracket_room - BRACE_GAP, top, bottom)),
            }
        }
        // the names of the voices of a staff stand above each other, around its middle
        for (place, top) in tops.iter().enumerate() {
            let names: Vec<(&String, Option<Span>)> = parts
                .iter()
                .zip(&labels)
                .filter(|(p, _)| p.staff == place)
                .filter_map(|(p, label)| label.as_ref().map(|l| (l, p.voice.span)))
                .collect();
            let count = names.len() as f32;
            for (i, (label, span)) in names.into_iter().enumerate() {
                let item = Item::Text {
                    x: x0 - group_room - 0.8,
                    y: top + 2.0 + LABEL_SIZE * 0.3 + (i as f32 - (count - 1.0) / 2.0) * LABEL_SIZE * 1.1,
                    text: label.clone(),
                    size: LABEL_SIZE,
                    anchor: Anchor::End,
                    style: TextStyle::Normal,
                };
                if let Some(span) = span {
                    page.hits.push(Hit {
                        rect: item.bounds(),
                        span,
                    });
                }
                page.items.push(item);
            }
        }
    }

    // W: lines: the words after the tune, as a block in the middle of the page
    let words: Vec<&InfoField> = tune
        .header
        .iter()
        .chain(tune.body.iter().filter_map(|l| match l {
            BodyLine::Field(f) => Some(f),
            _ => None,
        }))
        .filter(|f| f.name == 'W')
        .collect();
    if !words.is_empty() {
        let width = words
            .iter()
            .map(|f| crate::text_width(&f.value, WORDS_SIZE))
            .fold(0.0, f32::max);
        let x = ((opt.width - width) / 2.0).max(opt.margin);
        y += 1.0;
        for f in words {
            // an empty W: line leaves a gap between verses
            y += WORDS_SIZE * 1.3;
            if !f.value.is_empty() {
                text(page, x, y, &f.value, WORDS_SIZE, Anchor::Start, f.span);
            }
        }
        y += 1.0;
    }
    y
}

/// A voice while its tune is engraved.
struct Part<'a> {
    voice: Voice<'a>,
    /// What is sung on the notes of the voice.
    underlay: Underlay,
    /// The next of the voice's events to engrave.
    next: usize,
    state: State,
    carry: Carry,
    /// The next staff shows the time signature.
    show_meter: bool,
    /// The staff the voice is on, counted from the top.
    staff: usize,
    /// On a staff shared with other voices: which way its stems, slurs and
    /// marks go (see `Col::role`).
    role: Option<bool>,
}

/// The state a voice starts in: that of the tune, changed by its `V:` fields.
/// `clef` is the clef of its staff, if the voice or the staff's first voice
/// names one.
fn voice_state(tune: &State, props: &VoiceProps, clef: Option<Clef>) -> State {
    let mut state = tune.clone();
    if let Some(clef) = clef {
        state.clef = clef;
    }
    // `middle=` names the written pitch on the middle line
    let middle = state.clef.top_line() - 4;
    state.shift = props.middle.map(|m| middle - m).unwrap_or(0) + 7 * props.octave.unwrap_or(0);
    state.stem = props.stem;
    state
}

/// The music after one `&`. It starts with the bar it is written in.
struct OverlaySegment<'a> {
    /// Where the bar line before its bar starts in the source; `None` in the
    /// first bar of a line.
    bar: Option<usize>,
    /// The `&`.
    span: Span,
    symbols: Vec<&'a Symbol>,
}

/// Takes a line of music apart at its `&`s: the music of the voice itself,
/// and for each extra voice (after the first `&` of a bar, after the second,
/// and so on) what it plays in each bar that has it.
fn split_overlays<'a>(symbols: &[&'a Symbol]) -> (Vec<&'a Symbol>, Vec<Vec<OverlaySegment<'a>>>) {
    let mut music = vec![];
    let mut extra: Vec<Vec<OverlaySegment>> = vec![];
    // how many `&` the bar has had
    let mut depth = 0;
    let mut bar = None;
    for &sym in symbols {
        match &sym.kind {
            MusicSymbol::Bar(_) => {
                depth = 0;
                bar = Some(sym.span.start);
                music.push(sym);
            }
            MusicSymbol::Overlay => {
                depth += 1;
                if extra.len() < depth {
                    extra.push(vec![]);
                }
                extra[depth - 1].push(OverlaySegment {
                    bar,
                    span: sym.span,
                    symbols: vec![],
                });
            }
            _ if depth == 0 => music.push(sym),
            _ => {
                if let Some(segment) = extra[depth - 1].last_mut() {
                    segment.symbols.push(sym);
                }
            }
        }
    }
    (music, extra)
}

/// A staff for each extra voice that the `&`s of a line make, to be drawn on
/// the staff of `main`, the voice they are written in. In the bars that have
/// one, the stems of the voice and of the extra voice point away from each
/// other. `state` is the state of the voice at the start of the line. Music
/// after an `&` that is not as long as its bar is reported.
fn overlay_staves(
    main: &mut Staff,
    extra: &[Vec<OverlaySegment>],
    state: &State,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<Staff> {
    let up = main.role.unwrap_or(true);
    let mut staves = vec![];
    for (depth, segments) in extra.iter().enumerate() {
        let mut staff = Staff::new(state.clone());
        staff.place = main.place;
        staff.lead = false;
        staff.overlay = true;
        staff.role = Some(if depth % 2 == 0 { !up } else { up });
        let mut state = state.clone();
        for segment in segments {
            // its bar in the main voice: from the bar line before it to the next one
            let bars: Vec<&Col> = main.cols.iter().filter(|c| matches!(c.kind, Kind::Bar(_))).collect();
            let next = match segment.bar {
                Some(start) => bars.iter().position(|c| c.span.start == start).map(|i| i + 1),
                None => Some(0),
            };
            let Some(next) = next else { continue };
            let from = match next {
                0 => Rational64::from_integer(0),
                _ => bars[next - 1].onset,
            };
            let to = bars.get(next).map_or(main.duration(), |c| c.onset);
            let close = bars.get(next).map(|c| (c.kind.clone(), c.span));

            let first = staff.cols.len();
            staff.read(&segment.symbols, &mut state, &mut Carry::default());
            let Some(col) = staff.cols.get_mut(first) else {
                continue;
            };
            col.start = Some(from);
            staff.set_onsets();
            let end = staff.duration();
            // the bar line that ends the bar keeps its distance from this music too
            if let Some((kind, span)) = close {
                let mut bar = Col::new(kind, span, Rational32::from_integer(0));
                bar.start = Some(to);
                bar.hidden = true;
                staff.cols.push(bar);
                staff.set_onsets();
            }
            for c in main.cols.iter_mut() {
                if c.is_timed() && c.onset >= from && c.onset < to {
                    c.role = Some(up);
                }
            }
            if end != to {
                let span = segment.symbols.last().map_or(segment.span, |s| segment.span.join(s.span));
                let unit = wide(state.unit);
                diagnostics.push(Diagnostic::new(
                    span,
                    format!(
                        "the music after this & is {} units of L:{} long, its bar is {}",
                        (end - from) / unit,
                        state.unit,
                        (to - from) / unit
                    ),
                ));
            }
        }
        staves.push(staff);
    }
    staves
}

/// The source text of a line of music, without the spaces around it.
fn line_span(symbols: &[&Symbol]) -> Span {
    let mut spans = symbols
        .iter()
        .filter(|s| s.kind != MusicSymbol::VisualBreak)
        .map(|s| s.span);
    let first = spans.next().unwrap_or_default();
    spans.last().map(|last| first.join(last)).unwrap_or(first)
}

/// A `K:`, `M:` or `L:` line of the body.
fn apply_field(state: &mut State, show_meter: &mut bool, f: &InfoField) {
    match f.name {
        'K' => {
            let k = parse_key(&f.value);
            if !state.sharp_notation {
                state.fifths = k.fifths;
            }
            if let Some(c) = k.clef {
                state.set_clef(c);
            }
        }
        'M' => {
            state.meter = parse_meter(&f.value);
            *show_meter = true;
        }
        'L' => {
            if let Some(u) = parse_length(&f.value) {
                state.unit = u;
            }
        }
        _ => {}
    }
}

//------------------------------------------------------------------------------
// columns

#[derive(Clone, Debug)]
struct Head {
    d: i32,
    y: f32,
    acc: Option<Accidental>,
    tie: bool,
    /// Sharp notation: drawn with the raised notehead.
    raised: bool,
    /// Offset of the notehead from the column x (seconds in chords are displaced).
    dx: f32,
    acc_x: f32,
    /// Width of the notehead.
    w: f32,
    /// How far from the centre line the stem meets the notehead.
    attach: f32,
}

/// Where a notehead meets its stem.
struct HeadMetrics {
    width: f32,
    /// Left edge to the right edge of an up stem.
    up: f32,
    /// Left edge to the left edge of a down stem.
    down: f32,
    attach: f32,
}

fn head_metrics(value: &NoteValue, raised: bool) -> HeadMetrics {
    if raised {
        let s = value.raised_shape();
        if value.has_stem() {
            HeadMetrics { width: s.width(), up: s.stem_up_x(), down: s.stem_down_x(), attach: 0.0 }
        } else {
            HeadMetrics { width: s.width(), up: s.width(), down: 0.0, attach: 0.0 }
        }
    } else {
        let w = g::width(value.notehead());
        HeadMetrics { width: w, up: w, down: 0.0, attach: g::STEM_ATTACH_Y }
    }
}

#[derive(Clone, Debug)]
struct GraceNote {
    d: i32,
    acc: Option<Accidental>,
    raised: bool,
    value: NoteValue,
    span: Span,
}

#[derive(Clone, Debug, Default)]
struct Grace {
    notes: Vec<GraceNote>,
    acciaccatura: bool,
    width: f32,
}

#[derive(Clone, Debug)]
enum TextKind {
    Chord,
    Annotation(Placement),
}

#[derive(Clone, Debug)]
struct TextMark {
    kind: TextKind,
    text: String,
    span: Span,
}

#[derive(Clone, Debug)]
enum Kind {
    Note {
        heads: Vec<Head>,
        stem_up: bool,
        beam: Option<usize>,
    },
    Rest {
        visible: bool,
        measure: Option<i32>,
    },
    Bar(String),
    Key {
        fifths: i32,
        clef: Option<Clef>,
    },
    Meter(Meter),
}

#[derive(Clone, Debug)]
struct Col {
    kind: Kind,
    span: Span,
    dur: Rational32,
    value: NoteValue,
    decos: Vec<Decoration>,
    texts: Vec<TextMark>,
    grace: Option<Grace>,
    /// A space, bar or rest came before this column, so it starts a new beam.
    break_before: bool,
    /// Sounding time over written time: `q/p` inside a tuplet, otherwise 1.
    scale: Rational32,
    /// When the column starts, in whole notes from the start of the system.
    onset: Rational64,
    /// Its place among the columns of its staff that start at the same time.
    order: u32,
    /// Index of its moment in the system.
    moment: usize,
    /// Notes: the syllables under it, one for each verse that has one.
    under: Vec<Syllable>,
    /// How far the syllables reach to the left and to the right of `x`.
    lyric_left: f32,
    lyric_right: f32,
    /// On a staff shared with another voice: `Some(true)` when the stems,
    /// slurs and marks of the column go up, `Some(false)` when they go down.
    role: Option<bool>,
    /// The column starts at this time whatever came before it: the first
    /// column of the music after an `&`.
    start: Option<Rational64>,
    /// Another voice of the staff draws this bar line, signature or rest.
    hidden: bool,
    /// How far the column stands to the right of its moment, clear of the
    /// noteheads of another voice.
    offset: f32,
    /// How far its dots stand to the right of their usual place, clear of
    /// the noteheads of another voice.
    dots_x: f32,
    /// Rests: how far the rest is moved down, clear of another voice.
    rest_shift: f32,
    x: f32,
    left: f32,
    right: f32,
    space: f32,
    stem_x: f32,
    stem_end: f32,
    top: f32,
    bottom: f32,
    /// Notes: where the stem edge sits from `x` (see `HeadMetrics`).
    anchor: f32,
    /// Notes: centre of the noteheads that are not displaced, from `x`.
    mid: f32,
}

impl Col {
    fn new(kind: Kind, span: Span, dur: Rational32) -> Col {
        Col {
            kind,
            span,
            dur,
            value: NoteValue { log: -2, dots: 0 },
            decos: vec![],
            texts: vec![],
            grace: None,
            break_before: true,
            scale: Rational32::from_integer(1),
            onset: Rational64::from_integer(0),
            order: 0,
            moment: 0,
            under: vec![],
            lyric_left: 0.0,
            lyric_right: 0.0,
            role: None,
            start: None,
            hidden: false,
            offset: 0.0,
            dots_x: 0.0,
            rest_shift: 0.0,
            x: 0.0,
            left: 0.0,
            right: 0.0,
            space: 0.0,
            stem_x: 0.0,
            stem_end: 0.0,
            top: 0.0,
            bottom: 4.0,
            anchor: HEAD_W,
            mid: HEAD_W / 2.0,
        }
    }

    fn is_timed(&self) -> bool {
        matches!(self.kind, Kind::Note { .. } | Kind::Rest { .. })
    }

    /// How long the column sounds: its written duration, scaled inside a
    /// tuplet; a bar rest lasts all its bars.
    fn sounding(&self) -> Rational64 {
        let bars = match self.kind {
            Kind::Rest { measure: Some(n), .. } => n.max(1) as i64,
            _ => 1,
        };
        wide(self.dur) * wide(self.scale) * Rational64::from_integer(bars)
    }

    /// Columns with the same key, on any staff of a system, share a moment.
    fn key(&self) -> (Rational64, u32) {
        (self.onset, self.order)
    }

    fn heads(&self) -> &[Head] {
        match &self.kind {
            Kind::Note { heads, .. } => heads,
            _ => &[],
        }
    }

    fn stem_up(&self) -> bool {
        matches!(self.kind, Kind::Note { stem_up: true, .. })
    }

    fn has_stem(&self) -> bool {
        matches!(self.kind, Kind::Note { .. }) && self.value.has_stem()
    }

    fn head_top(&self) -> f32 {
        self.heads().iter().map(|h| h.y).fold(f32::MAX, f32::min)
    }

    fn head_bottom(&self) -> f32 {
        self.heads().iter().map(|h| h.y).fold(f32::MIN, f32::max)
    }

    /// Horizontal centre of the normal (not displaced) noteheads.
    fn center(&self) -> f32 {
        match self.kind {
            Kind::Note { .. } => self.x + self.mid,
            Kind::Rest { .. } => self.x + self.value.rest_width() / 2.0,
            _ => self.x + self.right / 2.0,
        }
    }
}

trait RestWidth {
    fn rest_width(&self) -> f32;
}

impl RestWidth for NoteValue {
    fn rest_width(&self) -> f32 {
        g::width(self.rest())
    }
}

struct TupletGroup {
    p: i32,
    cols: Vec<usize>,
}

/// A slur from `start` to `end`; `None` means the start or end of the system.
struct SlurMark {
    start: Option<usize>,
    end: Option<usize>,
}

struct EndingMark {
    n: i32,
    /// The bar the ending starts at, if it follows one.
    bar: Option<usize>,
    /// The first column of the ending.
    first: usize,
}

enum SlurEntry {
    /// `(` seen, waiting for its first note.
    Pending,
    Started(usize),
    /// Continued from the previous system.
    Carried,
}

// The order of columns that start at the same time: bar lines, then key and
// clef changes, then meter changes, then the note or rest. The gaps leave room
// for several of a kind, such as two bar lines.
const ORDER_BAR: u32 = 0;
const ORDER_KEY: u32 = 100;
const ORDER_METER: u32 = 200;
const ORDER_TIMED: u32 = u32::MAX;

/// A tune that writes any black key as I K N Q W is in sharp notation.
fn uses_sharp_notation(tune: &Tune) -> bool {
    tune.music_lines().flat_map(|l| &l.symbols).any(|s| match &s.kind {
        MusicSymbol::Stem { pitches, .. } => pitches.iter().any(|p| p.raised),
        _ => false,
    })
}

/// Horizontal step from one grace note to the next.
fn grace_step(raised: bool) -> f32 {
    if raised {
        0.95 + (crate::Shape::RaisedBlack.width() - HEAD_W) * GRACE
    } else {
        0.95
    }
}

fn staff_y(clef: Clef, d: i32) -> f32 {
    (clef.top_line() - d) as f32 * 0.5
}

fn accidental_glyph(a: Accidental) -> char {
    match a {
        Accidental::Natural => g::NATURAL,
        Accidental::Sharp => g::SHARP,
        Accidental::Flat => g::FLAT,
        Accidental::DoubleSharp => g::DOUBLE_SHARP,
        Accidental::DoubleFlat => g::DOUBLE_FLAT,
    }
}

/// Natural distance from one note to the next, by duration.
fn duration_space(d: Rational32) -> f32 {
    let d = *d.numer() as f32 / *d.denom() as f32;
    (3.4 * (4.0 * d).sqrt()).clamp(1.7, 9.0)
}

/// The same fraction with room for sums and products.
fn wide(r: Rational32) -> Rational64 {
    Rational64::new(*r.numer() as i64, *r.denom() as i64)
}

//------------------------------------------------------------------------------
// staves

/// The music of one voice on a staff of a system, in the staff's own
/// coordinates: x from its left end, y from its top line. Voices that share a
/// staff are each one of these, drawn at the same place.
struct Staff {
    start: State,
    /// The staff of the system this is drawn on, counted from the top.
    place: usize,
    /// Draws the staff itself: its lines, clef and signatures. The first
    /// voice of each staff does.
    lead: bool,
    /// The music after the `&`s of a voice.
    overlay: bool,
    /// The role its notes and rests get (see `Col::role`).
    role: Option<bool>,
    cols: Vec<Col>,
    beams: Vec<Vec<usize>>,
    tuplets: Vec<TupletGroup>,
    slurs: Vec<SlurMark>,
    endings: Vec<EndingMark>,
    items: Vec<Item>,
    hits: Vec<Hit>,
    /// Where the key signature starts, after the clef.
    key_x: f32,
    preamble_end: f32,
    staff_end: f32,
    meter_x: Option<f32>,
}

impl Staff {
    fn new(start: State) -> Staff {
        Staff {
            start,
            place: 0,
            lead: true,
            overlay: false,
            role: None,
            cols: vec![],
            beams: vec![],
            tuplets: vec![],
            slurs: vec![],
            endings: vec![],
            items: vec![],
            hits: vec![],
            key_x: 0.0,
            preamble_end: 0.0,
            staff_end: 0.0,
            meter_x: None,
        }
    }

    //--------------------------------------------------------------------------
    // pass 1: symbols to columns

    fn read(&mut self, symbols: &[&Symbol], state: &mut State, carry: &mut Carry) {
        let mut decos: Vec<Decoration> = vec![];
        let mut texts: Vec<TextMark> = vec![];
        let mut brk = true;
        let mut grace: Option<Grace> = None;
        let mut grace_ready: Option<Grace> = None;
        let mut broken: Option<(bool, u8, usize)> = None;
        let mut last_timed: Option<usize> = None;
        // (tuplet, notes still to come, sounding time over written time)
        let mut tuplet: Option<(usize, i32, Rational32)> = None;
        let mut slurs: Vec<SlurEntry> = (0..carry.slurs).map(|_| SlurEntry::Carried).collect();

        for sym in symbols {
            let mut timed: Option<usize> = None;
            match &sym.kind {
                MusicSymbol::Stem {
                    pitches,
                    length,
                    decorations,
                    tie,
                } => {
                    let dur = *length * state.unit;
                    if let Some(gr) = grace.as_mut() {
                        if let Some(p) = pitches.first() {
                            gr.notes.push(GraceNote {
                                d: p.diatonic() + state.shift,
                                acc: p.accidental,
                                raised: p.raised,
                                value: note_value(dur),
                                span: sym.span,
                            });
                        }
                        continue;
                    }
                    let mut heads: Vec<Head> = pitches
                        .iter()
                        .map(|p| Head {
                            d: p.diatonic() + state.shift,
                            y: staff_y(state.clef, p.diatonic() + state.shift),
                            acc: p.accidental,
                            tie: p.tie.is_some() || tie.is_some(),
                            raised: p.raised,
                            dx: 0.0,
                            acc_x: 0.0,
                            w: HEAD_W,
                            attach: g::STEM_ATTACH_Y,
                        })
                        .collect();
                    heads.sort_by_key(|h| h.d);
                    let mut col = Col::new(
                        Kind::Note {
                            heads,
                            stem_up: true,
                            beam: None,
                        },
                        sym.span,
                        dur,
                    );
                    col.decos = std::mem::take(&mut decos);
                    col.decos.extend(decorations.iter().cloned());
                    col.texts = std::mem::take(&mut texts);
                    col.grace = grace_ready.take();
                    col.role = self.role;
                    col.break_before = brk;
                    brk = false;
                    self.cols.push(col);
                    timed = Some(self.cols.len() - 1);
                }
                MusicSymbol::Rest(r) => {
                    let bar = state.meter.value().unwrap_or(Rational32::from_integer(1));
                    let (visible, measure, dur) = match r {
                        Rest::Note(l) => (true, None, *l * state.unit),
                        Rest::NoteHidden(l) => (false, None, *l * state.unit),
                        Rest::Measure(n) => (true, Some(*n), bar),
                        Rest::MeasureHidden(n) => (false, Some(*n), bar),
                    };
                    let mut col = Col::new(Kind::Rest { visible, measure }, sym.span, dur);
                    col.decos = std::mem::take(&mut decos);
                    col.texts = std::mem::take(&mut texts);
                    col.role = self.role;
                    self.cols.push(col);
                    brk = true;
                    timed = Some(self.cols.len() - 1);
                }
                MusicSymbol::Bar(b) => {
                    let mut col = Col::new(Kind::Bar(b.clone()), sym.span, Rational32::from_integer(0));
                    col.decos = std::mem::take(&mut decos);
                    col.texts = std::mem::take(&mut texts);
                    self.cols.push(col);
                    brk = true;
                }
                MusicSymbol::Ending(n) => {
                    let bar = match self.cols.last() {
                        Some(Col { kind: Kind::Bar(_), .. }) => Some(self.cols.len() - 1),
                        _ => None,
                    };
                    self.endings.push(EndingMark {
                        n: *n,
                        bar,
                        first: self.cols.len(),
                    });
                }
                MusicSymbol::VisualBreak => brk = true,
                MusicSymbol::SlurBegin => slurs.push(SlurEntry::Pending),
                MusicSymbol::SlurEnd => match slurs.pop() {
                    Some(SlurEntry::Started(s)) => self.slurs.push(SlurMark {
                        start: Some(s),
                        end: last_timed.filter(|e| *e > s),
                    }),
                    Some(SlurEntry::Carried) => self.slurs.push(SlurMark {
                        start: None,
                        end: last_timed,
                    }),
                    Some(SlurEntry::Pending) | None => {}
                },
                MusicSymbol::TupletStart { p, q, r } => {
                    self.tuplets.push(TupletGroup {
                        p: *p,
                        cols: vec![],
                    });
                    let q = q.unwrap_or_else(|| default_tuplet_q(*p, state.meter.is_compound()));
                    tuplet = Some((self.tuplets.len() - 1, r.unwrap_or(*p), Rational32::new(q, *p)));
                }
                MusicSymbol::GraceStart { acciaccatura } => {
                    grace = Some(Grace {
                        acciaccatura: *acciaccatura,
                        ..Grace::default()
                    })
                }
                MusicSymbol::GraceEnd => grace_ready = grace.take().filter(|g| !g.notes.is_empty()),
                MusicSymbol::BrokenRhythm { first_longer, dots } => {
                    broken = last_timed.map(|i| (*first_longer, *dots, i));
                }
                MusicSymbol::ChordSymbol(t) => texts.push(TextMark {
                    kind: TextKind::Chord,
                    text: t.clone(),
                    span: sym.span,
                }),
                MusicSymbol::Annotation { placement, text } => texts.push(TextMark {
                    kind: TextKind::Annotation(*placement),
                    text: text.clone(),
                    span: sym.span,
                }),
                MusicSymbol::InlineField(f) => match f.name {
                    'K' => {
                        let k = parse_key(&f.value);
                        if !state.sharp_notation {
                            state.fifths = k.fifths;
                        }
                        if let Some(c) = k.clef {
                            state.set_clef(c);
                        }
                        self.cols.push(Col::new(
                            Kind::Key {
                                fifths: state.fifths,
                                clef: k.clef,
                            },
                            sym.span,
                            Rational32::from_integer(0),
                        ));
                        brk = true;
                    }
                    'M' => {
                        state.meter = parse_meter(&f.value);
                        self.cols.push(Col::new(
                            Kind::Meter(state.meter.clone()),
                            sym.span,
                            Rational32::from_integer(0),
                        ));
                        brk = true;
                    }
                    'L' => {
                        if let Some(u) = parse_length(&f.value) {
                            state.unit = u;
                        }
                    }
                    _ => {}
                },
                MusicSymbol::Decorations(d) => decos.extend(d.iter().cloned()),
                // a line is taken apart at its `&`s before it is read
                MusicSymbol::Overlay | MusicSymbol::Unknown(_) => {}
            }

            if let Some(idx) = timed {
                if let Some((first_longer, dots, prev)) = broken.take() {
                    let short = Rational32::new(1, 1 << dots.min(3));
                    let long = Rational32::from_integer(2) - short;
                    let (a, b) = if first_longer { (long, short) } else { (short, long) };
                    self.cols[prev].dur *= a;
                    self.cols[idx].dur *= b;
                }
                if let Some((t, remaining, scale)) = tuplet.as_mut() {
                    self.tuplets[*t].cols.push(idx);
                    self.cols[idx].scale = *scale;
                    *remaining -= 1;
                    if *remaining <= 0 {
                        tuplet = None;
                    }
                }
                if matches!(self.cols[idx].kind, Kind::Note { .. }) {
                    for s in slurs.iter_mut() {
                        if matches!(s, SlurEntry::Pending) {
                            *s = SlurEntry::Started(idx);
                        }
                    }
                }
                last_timed = Some(idx);
            }
        }

        carry.slurs = 0;
        for s in slurs {
            match s {
                SlurEntry::Started(st) => {
                    self.slurs.push(SlurMark {
                        start: Some(st),
                        end: None,
                    });
                    carry.slurs += 1;
                }
                SlurEntry::Carried => {
                    self.slurs.push(SlurMark { start: None, end: None });
                    carry.slurs += 1;
                }
                SlurEntry::Pending => {}
            }
        }
        self.set_onsets();
    }

    /// Gives every column the time it starts at and its place among the
    /// columns that start at that time. Broken rhythm changes a duration after
    /// the next note has been read, so this runs once all columns are known.
    fn set_onsets(&mut self) {
        let mut time = Rational64::from_integer(0);
        let mut order = 0;
        for c in self.cols.iter_mut() {
            if let Some(start) = c.start {
                time = start;
                order = 0;
            }
            c.onset = time;
            if c.is_timed() {
                c.order = ORDER_TIMED;
                time += c.sounding();
                order = 0;
            } else {
                let first = match c.kind {
                    Kind::Key { .. } => ORDER_KEY,
                    Kind::Meter(_) => ORDER_METER,
                    _ => ORDER_BAR,
                };
                c.order = first.max(order);
                order = c.order + 1;
            }
        }
    }

    //--------------------------------------------------------------------------
    /// How long the music of the staff sounds, in whole notes.
    fn duration(&self) -> Rational64 {
        match self.cols.last() {
            Some(c) if c.is_timed() => c.onset + c.sounding(),
            Some(c) => c.onset,
            None => Rational64::from_integer(0),
        }
    }

    //--------------------------------------------------------------------------
    /// Puts the syllables of the voice under the notes of this staff.
    fn set_lyrics(&mut self, underlay: &Underlay) {
        for c in self.cols.iter_mut() {
            if matches!(c.kind, Kind::Note { .. })
                && let Some(under) = underlay.get(&c.span.start)
            {
                c.under = under.clone();
            }
        }
    }

    //--------------------------------------------------------------------------
    // pass 2: values, beams, stems, widths

    fn prepare(&mut self, show_meter: bool) {
        let middle = self.start.clef.top_line() - 4;

        for col in self.cols.iter_mut() {
            col.value = match col.kind {
                Kind::Rest { measure: Some(_), .. } => NoteValue { log: 0, dots: 0 },
                _ => note_value(col.dur),
            };
        }

        // beams: consecutive short notes with nothing in between
        let mut group: Vec<usize> = vec![];
        let mut groups: Vec<Vec<usize>> = vec![];
        for i in 0..self.cols.len() {
            let c = &self.cols[i];
            let beamable = matches!(c.kind, Kind::Note { .. }) && c.value.flags() > 0;
            if beamable && !c.break_before && group.last() == Some(&(i.wrapping_sub(1))) {
                group.push(i);
            } else {
                if group.len() >= 2 {
                    groups.push(std::mem::take(&mut group));
                }
                group = if beamable { vec![i] } else { vec![] };
            }
        }
        if group.len() >= 2 {
            groups.push(group);
        }
        for (id, gr) in groups.iter().enumerate() {
            for &i in gr {
                if let Kind::Note { beam, .. } = &mut self.cols[i].kind {
                    *beam = Some(id);
                }
            }
        }

        // stem directions: away from the note furthest from the middle line
        let reach = |c: &Col| -> (i32, i32) {
            let ds: Vec<i32> = c.heads().iter().map(|h| h.d).collect();
            let top = ds.iter().copied().max().unwrap_or(middle);
            let bottom = ds.iter().copied().min().unwrap_or(middle);
            (top - middle, middle - bottom)
        };
        for gr in &groups {
            let (above, below) = gr.iter().fold((i32::MIN, i32::MIN), |(a, b), &i| {
                let (x, y) = reach(&self.cols[i]);
                (a.max(x), b.max(y))
            });
            for &i in gr {
                if let Kind::Note { stem_up, .. } = &mut self.cols[i].kind {
                    *stem_up = below > above;
                }
            }
        }
        for c in self.cols.iter_mut() {
            let (above, below) = reach(c);
            if let Kind::Note { stem_up, beam: None, .. } = &mut c.kind {
                *stem_up = below > above;
            }
        }
        if let Some(up) = self.start.stem {
            for c in self.cols.iter_mut() {
                if let Kind::Note { stem_up, .. } = &mut c.kind {
                    *stem_up = up;
                }
            }
        }
        // on a staff shared with another voice the stems show which voice is which
        for c in self.cols.iter_mut() {
            if let (Some(up), Kind::Note { stem_up, .. }) = (c.role, &mut c.kind) {
                *stem_up = up;
            }
        }
        self.beams = groups;

        // widths of each column
        for c in self.cols.iter_mut() {
            Self::measure(c);
            if c.under.is_empty() {
                continue;
            }
            // the syllables are centred under the note
            let mut half = 0.0_f32;
            let mut hyphen = 0.0;
            for s in &c.under {
                if let Sung::Text { text, hyphen: goes_on } = &s.sung {
                    half = half.max(crate::text_width(text, LYRIC_SIZE) / 2.0);
                    if *goes_on {
                        hyphen = crate::text_width("-", LYRIC_SIZE) + 0.4;
                    }
                }
            }
            c.lyric_left = half - c.mid;
            c.lyric_right = half + c.mid + hyphen;
        }

        // preamble: clef, key signature, time signature
        let clef = self.start.clef;
        let mut x = 0.6 + g::width(clef.glyph()) + 0.7;
        self.key_x = x;
        let key = clef.key_positions(self.start.fifths);
        if !key.is_empty() {
            let w = if self.start.fifths > 0 { g::width(g::SHARP) } else { g::width(g::FLAT) };
            x += key.len() as f32 * (w + 0.12) + 0.5;
        }
        if show_meter && self.start.meter != Meter::None {
            self.meter_x = Some(x);
            x += meter_width(&self.start.meter) + 1.0;
        }
        self.preamble_end = x + 0.4;
    }

    /// The natural distance from column `i` to the next column of this staff.
    fn gap_after(&self, i: usize) -> f32 {
        let c = &self.cols[i];
        let next = &self.cols[i + 1];
        let before_bar = if c.is_timed() && matches!(next.kind, Kind::Bar(_)) { 0.9 } else { 0.0 };
        (c.space + before_bar).max(c.offset + c.right + 0.6 + next.left - next.offset)
    }

    /// Sets the left and right extent and natural spacing of a column.
    fn measure(c: &mut Col) {
        match &mut c.kind {
            Kind::Note { heads, stem_up, .. } => {
                let up = *stem_up || !c.value.has_stem();
                let value = c.value;
                // line the heads up on the stem; heads of different shapes meet it at different points
                let metrics: Vec<HeadMetrics> = heads.iter().map(|h| head_metrics(&value, h.raised)).collect();
                let anchor = if up {
                    metrics.iter().map(|m| m.up).fold(0.0, f32::max)
                } else {
                    metrics.iter().map(|m| m.down).fold(0.0, f32::max)
                };
                for (h, m) in heads.iter_mut().zip(&metrics) {
                    h.w = m.width;
                    h.attach = m.attach;
                    h.dx = if up { anchor - m.up } else { anchor - m.down };
                }
                c.anchor = anchor;
                let min_dx = heads.iter().map(|h| h.dx).fold(f32::MAX, f32::min);
                let max_right = heads.iter().map(|h| h.dx + h.w).fold(f32::MIN, f32::max);
                c.mid = (min_dx + max_right) / 2.0;

                // seconds and unisons (C and I) in chords: displace every other head to the far side of the stem
                if up {
                    let mut prev: Option<i32> = None;
                    for (h, m) in heads.iter_mut().zip(&metrics) {
                        if prev == Some(h.d - 1) || prev == Some(h.d) {
                            h.dx = anchor - g::STEM - m.down;
                            prev = None;
                        } else {
                            prev = Some(h.d);
                        }
                    }
                } else {
                    let mut prev: Option<i32> = None;
                    for (h, m) in heads.iter_mut().zip(&metrics).rev() {
                        if prev == Some(h.d + 1) || prev == Some(h.d) {
                            h.dx = anchor + g::STEM - m.up;
                            prev = None;
                        } else {
                            prev = Some(h.d);
                        }
                    }
                }
                let min_dx = heads.iter().map(|h| h.dx).fold(f32::MAX, f32::min);
                let max_right = heads.iter().map(|h| h.dx + h.w).fold(f32::MIN, f32::max);

                // accidentals in columns, highest first, avoiding clashes
                let mut columns: Vec<Vec<i32>> = vec![];
                let mut col_width: Vec<f32> = vec![];
                let mut order: Vec<usize> = (0..heads.len()).filter(|&i| heads[i].acc.is_some()).collect();
                order.sort_by_key(|&i| -heads[i].d);
                let mut placed: Vec<(usize, usize)> = vec![];
                for i in order {
                    let d = heads[i].d;
                    let w = g::width(accidental_glyph(heads[i].acc.unwrap()));
                    let ci = columns
                        .iter()
                        .position(|ds| ds.iter().all(|&o| (o - d).abs() >= 6))
                        .unwrap_or(columns.len());
                    if ci == columns.len() {
                        columns.push(vec![]);
                        col_width.push(0.0);
                    }
                    columns[ci].push(d);
                    col_width[ci] = col_width[ci].max(w);
                    placed.push((i, ci));
                }
                let mut acc_left = 0.0;
                let mut col_x = vec![];
                let mut x = min_dx - 0.2;
                for w in &col_width {
                    x -= w;
                    col_x.push(x);
                    x -= 0.12;
                    acc_left = -x;
                }
                for (i, ci) in placed {
                    heads[i].acc_x = col_x[ci];
                }

                let mut left = acc_left.max(-min_dx);
                if let Some(gr) = c.grace.as_mut() {
                    let w: f32 = gr
                        .notes
                        .iter()
                        .map(|n| grace_step(n.raised) + if n.acc.is_some() { 0.7 } else { 0.0 })
                        .sum::<f32>()
                        + 0.3;
                    gr.width = w;
                    left += w;
                }

                let mut right = max_right;
                if c.value.dots > 0 {
                    right += 0.3 + c.value.dots as f32 * 0.55;
                }
                let flags = c.value.flags();
                if flags > 0 && matches!(c.kind, Kind::Note { beam: None, .. }) {
                    right = right.max(if up { anchor - g::STEM + 1.06 } else { anchor + 1.25 });
                }
                c.left = left;
                c.right = right;
                c.space = duration_space(c.dur);
            }
            Kind::Rest { visible, .. } => {
                let w = if *visible { c.value.rest_width() } else { 1.0 };
                c.left = 0.0;
                c.right = w + if c.value.dots > 0 { 0.3 + c.value.dots as f32 * 0.55 } else { 0.0 };
                c.space = duration_space(c.dur).min(5.0);
            }
            Kind::Bar(b) => {
                c.left = 0.0;
                let parts = bar_parts(b);
                c.right = parts.iter().map(|p| p.width()).sum::<f32>()
                    - parts.last().map(|p| p.separation()).unwrap_or(0.0);
                c.space = c.right + 1.5;
            }
            Kind::Key { fifths, .. } => {
                let n = fifths.unsigned_abs().min(7) as f32;
                c.left = 0.0;
                c.right = n * 1.12 + 0.5;
                c.space = c.right + 1.0;
            }
            Kind::Meter(m) => {
                c.left = 0.0;
                c.right = meter_width(m);
                c.space = c.right + 1.2;
            }
        }
    }

    //--------------------------------------------------------------------------
    // pass 4: drawing

    fn glyph(&mut self, x: f32, y: f32, glyph: char, scale: f32) {
        self.items.push(Item::Glyph { x, y, glyph, scale });
    }

    fn line(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, width: f32) {
        self.items.push(Item::Line { x1, y1, x2, y2, width });
    }

    fn text(&mut self, x: f32, y: f32, text: &str, size: f32, anchor: Anchor, style: TextStyle, span: Option<Span>) {
        let item = Item::Text {
            x,
            y,
            text: text.to_string(),
            size,
            anchor,
            style,
        };
        if let Some(span) = span {
            self.hits.push(Hit {
                rect: item.bounds(),
                span,
            });
        }
        self.items.push(item);
    }

    fn draw(&mut self, carry: &mut Carry) {
        if self.lead {
            for i in 0..5 {
                let y = i as f32;
                self.line(0.0, y, self.staff_end, y, g::STAFF_LINE);
            }
            self.draw_preamble();
        }

        for i in 0..self.cols.len() {
            self.draw_column(i);
        }
        for b in 0..self.beams.len() {
            self.draw_beam(b);
        }
        for i in 0..self.cols.len() {
            self.finish_column(i);
        }
        self.draw_ties(carry);
        self.draw_tuplets();
        self.draw_slurs();
        for i in 0..self.cols.len() {
            self.draw_decorations(i);
        }
        self.draw_texts();
        self.draw_endings();
    }

    fn draw_preamble(&mut self) {
        let clef = self.start.clef;
        self.glyph(0.6, clef.glyph_y(), clef.glyph(), 1.0);
        self.draw_key(self.key_x, self.start.fifths, clef);
        if let Some(mx) = self.meter_x {
            let m = self.start.meter.clone();
            self.draw_meter(mx, &m);
        }
    }

    fn draw_key(&mut self, mut x: f32, fifths: i32, clef: Clef) -> f32 {
        let glyph = if fifths > 0 { g::SHARP } else { g::FLAT };
        for d in clef.key_positions(fifths) {
            self.glyph(x, staff_y(clef, d), glyph, 1.0);
            x += g::width(glyph) + 0.12;
        }
        x
    }

    fn draw_meter(&mut self, x: f32, m: &Meter) {
        let w = meter_width(m);
        match m {
            Meter::None => {}
            Meter::Common => self.glyph(x, 2.0, g::TIME_SIG_COMMON, 1.0),
            Meter::Cut => self.glyph(x, 2.0, g::TIME_SIG_CUT, 1.0),
            Meter::Fraction { num, den } => {
                let top: Vec<char> = meter_glyphs(&num.iter().map(|n| n.to_string()).collect::<Vec<_>>().join("+"));
                let bottom: Vec<char> = meter_glyphs(&den.to_string());
                for (glyphs, y) in [(top, 1.0), (bottom, 3.0)] {
                    let gw: f32 = glyphs.iter().map(|c| g::width(*c)).sum();
                    let mut gx = x + (w - gw) / 2.0;
                    for c in glyphs {
                        self.glyph(gx, y, c, 1.0);
                        gx += g::width(c);
                    }
                }
            }
        }
    }

    fn draw_column(&mut self, i: usize) {
        let c = self.cols[i].clone();
        let clef = self.start.clef;
        match &c.kind {
            Kind::Note { heads, stem_up, beam } => {
                let head = c.value.notehead();
                for h in heads {
                    if h.raised {
                        self.items.push(Item::Shape {
                            x: c.x + h.dx,
                            y: h.y,
                            shape: c.value.raised_shape(),
                            scale: 1.0,
                        });
                    } else {
                        self.glyph(c.x + h.dx, h.y, head, 1.0);
                    }
                    if let Some(a) = h.acc {
                        self.glyph(c.x + h.acc_x, h.y, accidental_glyph(a), 1.0);
                    }
                }
                self.ledger_lines(c.x, heads, 1.0);

                // dots, nudged into a space when the note sits on a line: the
                // space above, or the space below for the lower voice of a staff
                if c.value.dots > 0 {
                    let max_right = heads.iter().map(|h| h.dx + h.w).fold(0.0, f32::max) + c.dots_x;
                    let nudge = if c.role == Some(false) { 0.5 } else { -0.5 };
                    let mut ys: Vec<f32> = heads
                        .iter()
                        .map(|h| if h.y.fract() == 0.0 { h.y + nudge } else { h.y })
                        .collect();
                    ys.dedup();
                    for y in ys {
                        for k in 0..c.value.dots {
                            self.glyph(c.x + max_right + 0.3 + k as f32 * 0.55, y, g::AUGMENTATION_DOT, 1.0);
                        }
                    }
                }

                // stem end for notes without beams; beams set it later
                let top = c.head_top();
                let bottom = c.head_bottom();
                let col = &mut self.cols[i];
                col.stem_x = if *stem_up { c.x + c.anchor - g::STEM / 2.0 } else { c.x + c.anchor + g::STEM / 2.0 };
                let extra = (c.value.flags() as f32 - 2.0).max(0.0) * 0.75;
                col.stem_end = if *stem_up {
                    (top - g::STEM_LENGTH - extra).min(2.0)
                } else {
                    (bottom + g::STEM_LENGTH + extra).max(2.0)
                };
                let _ = beam;

                // grace notes
                if let Some(gr) = &c.grace {
                    let acc_left = heads
                        .iter()
                        .filter(|h| h.acc.is_some())
                        .map(|h| -h.acc_x)
                        .fold(0.0, f32::max);
                    let gx = c.x - acc_left.max(c.offset) - gr.width - 0.1;
                    self.draw_grace(gx, gr, clef);
                }

                let mut rect = Rect::new(c.x + c.mid - 0.7, top - 0.5, c.x + c.mid + 0.7, bottom + 0.5);
                for h in heads {
                    rect = rect.union(&Rect::new(c.x + h.dx, h.y - 0.5, c.x + h.dx + h.w, h.y + 0.5));
                    if h.acc.is_some() {
                        rect = rect.union(&Rect::new(c.x + h.acc_x, h.y - 1.0, c.x, h.y + 1.0));
                    }
                }
                self.hits.push(Hit { rect, span: c.span });
            }
            Kind::Rest { visible, measure } => {
                let glyph = c.value.rest();
                let y = if glyph == g::REST_WHOLE { 1.0 } else { 2.0 } + c.rest_shift;
                if *visible && !c.hidden {
                    self.glyph(c.x, y, glyph, 1.0);
                    if (glyph == g::REST_WHOLE || glyph == g::REST_HALF) && !(0.0..=4.0).contains(&y) {
                        let (x0, x1) = (c.x - g::LEDGER_EXTENSION, c.x + c.value.rest_width() + g::LEDGER_EXTENSION);
                        self.line(x0, y, x1, y, g::LEDGER_LINE);
                    }
                    for k in 0..c.value.dots {
                        let x = c.x + c.value.rest_width() + 0.3 + k as f32 * 0.55;
                        self.glyph(x, 1.5 + c.rest_shift, g::AUGMENTATION_DOT, 1.0);
                    }
                    if let Some(n) = measure.filter(|n| *n > 1) {
                        let digits = meter_glyphs(&n.to_string());
                        let w: f32 = digits.iter().map(|d| g::width(*d)).sum();
                        let mut x = c.x + c.value.rest_width() / 2.0 - w / 2.0;
                        for d in digits {
                            self.glyph(x, -1.5, d, 1.0);
                            x += g::width(d);
                        }
                    }
                }
                let (up, down) = g::extent(glyph);
                let col = &mut self.cols[i];
                col.top = (y - up).min(1.0);
                col.bottom = (y + down).max(3.0);
                self.hits.push(Hit {
                    rect: Rect::new(c.x - 0.1, y - up.max(0.6), c.x + c.right + 0.1, y + down.max(0.6)),
                    span: c.span,
                });
            }
            Kind::Bar(b) => {
                let mut x = c.x;
                let parts = if c.hidden { vec![] } else { bar_parts(b) };
                let n = parts.len();
                for (k, p) in parts.into_iter().enumerate() {
                    match p {
                        BarPart::Thin => self.line(x + g::THIN_BARLINE / 2.0, 0.0, x + g::THIN_BARLINE / 2.0, 4.0, g::THIN_BARLINE),
                        BarPart::Thick => self.line(x + g::THICK_BARLINE / 2.0, 0.0, x + g::THICK_BARLINE / 2.0, 4.0, g::THICK_BARLINE),
                        BarPart::Dots => {
                            self.glyph(x, 1.5, g::REPEAT_DOT, 1.0);
                            self.glyph(x, 2.5, g::REPEAT_DOT, 1.0);
                        }
                    }
                    x += p.width();
                    let _ = (k, n);
                }
                self.hits.push(Hit {
                    rect: Rect::new(c.x - 0.3, -0.2, c.x + c.right + 0.3, 4.2),
                    span: c.span,
                });
            }
            Kind::Key { fifths, clef } => {
                let mut x = c.x;
                if !c.hidden {
                    if let Some(cl) = clef {
                        self.glyph(x, cl.glyph_y(), cl.glyph(), 0.75);
                        x += g::width(cl.glyph()) * 0.75 + 0.5;
                    }
                    let cl = clef.unwrap_or(self.start.clef);
                    self.draw_key(x, *fifths, cl);
                }
                self.hits.push(Hit {
                    rect: Rect::new(c.x, -0.5, c.x + c.right, 4.5),
                    span: c.span,
                });
            }
            Kind::Meter(m) => {
                if !c.hidden {
                    self.draw_meter(c.x, m);
                }
                self.hits.push(Hit {
                    rect: Rect::new(c.x, 0.0, c.x + c.right, 4.0),
                    span: c.span,
                });
            }
        }
    }

    fn ledger_lines(&mut self, x: f32, heads: &[Head], scale: f32) {
        let ext = g::LEDGER_EXTENSION * scale;
        let min_dx = heads.iter().map(|h| h.dx).fold(f32::MAX, f32::min);
        let max_right = heads.iter().map(|h| h.dx + h.w).fold(f32::MIN, f32::max);
        let top = heads.iter().map(|h| h.y).fold(f32::MAX, f32::min);
        let bottom = heads.iter().map(|h| h.y).fold(f32::MIN, f32::max);
        let x0 = x + min_dx - ext;
        let x1 = x + max_right + ext;
        let mut y = -1.0;
        while y >= top - 0.01 {
            self.line(x0, y, x1, y, g::LEDGER_LINE);
            y -= 1.0;
        }
        let mut y = 5.0;
        while y <= bottom + 0.01 {
            self.line(x0, y, x1, y, g::LEDGER_LINE);
            y += 1.0;
        }
    }

    fn draw_grace(&mut self, mut x: f32, gr: &Grace, clef: Clef) {
        let s = GRACE;
        let n = gr.notes.len();
        let mut stems: Vec<(f32, f32, f32)> = vec![];
        let black = NoteValue { log: -2, dots: 0 };
        for note in &gr.notes {
            let y = staff_y(clef, note.d);
            if let Some(a) = note.acc {
                let glyph = accidental_glyph(a);
                self.glyph(x, y, glyph, s);
                x += g::width(glyph) * s + 0.1;
            }
            let m = head_metrics(&black, note.raised);
            if note.raised {
                self.items.push(Item::Shape { x, y, shape: crate::Shape::RaisedBlack, scale: s });
            } else {
                self.glyph(x, y, g::NOTEHEAD_BLACK, s);
            }
            let head = Head {
                d: note.d,
                y,
                acc: None,
                tie: false,
                raised: note.raised,
                dx: 0.0,
                acc_x: 0.0,
                w: m.width * s,
                attach: m.attach * s,
            };
            self.ledger_lines(x, &[head], s);
            self.hits.push(Hit {
                rect: Rect::new(x - 0.1, y - 0.5, x + m.width * s + 0.1, y + 0.5),
                span: note.span,
            });
            stems.push((x + m.up * s - g::STEM * s / 2.0, y, m.attach * s));
            x += grace_step(note.raised);
        }
        // stems up; one note gets a flag, several get beams
        let stem_len = 2.5;
        if n == 1 {
            let (sx, y, attach) = stems[0];
            let end = y - stem_len;
            self.line(sx, y - attach, sx, end, g::STEM * s);
            let flags = gr.notes[0].value.flags().clamp(1, 4);
            self.glyph(sx - g::STEM * s / 2.0, end, g::FLAG_UP[flags as usize - 1], s);
            if gr.acciaccatura {
                self.line(sx - 0.55, end + 1.5, sx + 0.75, end + 0.45, 0.1);
            }
        } else {
            let levels = gr.notes.iter().map(|n| n.value.flags()).max().unwrap_or(1).clamp(1, 3);
            let top = stems.iter().map(|s| s.1).fold(f32::MAX, f32::min);
            let x0 = stems[0].0;
            let x1 = stems[n - 1].0;
            let rise = ((stems[n - 1].1 - stems[0].1) * 0.3).clamp(-0.6, 0.6);
            let slope = if x1 > x0 { rise / (x1 - x0) } else { 0.0 };
            let mut e0 = f32::MAX;
            for &(sx, y, _) in &stems {
                e0 = e0.min(y - stem_len - slope * (sx - x0));
            }
            e0 = e0.min(top - stem_len);
            let b = |x: f32| e0 + slope * (x - x0);
            for &(sx, y, attach) in &stems {
                self.line(sx, y - attach, sx, b(sx), g::STEM * s);
            }
            let t = g::BEAM * s;
            for k in 0..levels {
                let off = k as f32 * (g::BEAM + g::BEAM_SPACING) * s;
                let xa = x0 - g::STEM * s / 2.0;
                let xb = x1 + g::STEM * s / 2.0;
                self.items.push(Item::Polygon {
                    points: vec![(xa, b(xa) + off), (xb, b(xb) + off), (xb, b(xb) + off + t), (xa, b(xa) + off + t)],
                });
            }
            if gr.acciaccatura {
                let sx = x0;
                let e = b(sx);
                self.line(sx - 0.55, e + 1.5, sx + 0.75, e + 0.45, 0.1);
            }
        }
    }

    fn draw_beam(&mut self, b: usize) {
        let group = self.beams[b].clone();
        let n = group.len();
        let up = self.cols[group[0]].stem_up();
        let xs: Vec<f32> = group.iter().map(|&i| self.cols[i].stem_x).collect();
        let tips: Vec<f32> = group
            .iter()
            .map(|&i| if up { self.cols[i].head_top() } else { self.cols[i].head_bottom() })
            .collect();
        let flags: Vec<u8> = group.iter().map(|&i| self.cols[i].value.flags()).collect();
        let levels = flags.iter().copied().max().unwrap_or(1);

        let x0 = xs[0];
        let dx = xs[n - 1] - x0;
        let rise = ((tips[n - 1] - tips[0]) * 0.5).clamp(-1.0, 1.0);
        // flat beam when the inner notes reach beyond both ends
        let inner_extreme = tips[1..n - 1].iter().any(|&t| {
            if up {
                t < tips[0].min(tips[n - 1])
            } else {
                t > tips[0].max(tips[n - 1])
            }
        });
        let slope = if dx > 0.0 && !inner_extreme { rise / dx } else { 0.0 };
        let extra = (levels as f32 - 2.0).max(0.0) * 0.75;
        let mut e0 = if up { f32::MAX } else { f32::MIN };
        for k in 0..n {
            let base = if up {
                tips[k] - g::STEM_LENGTH - extra
            } else {
                tips[k] + g::STEM_LENGTH + extra
            } - slope * (xs[k] - x0);
            e0 = if up { e0.min(base) } else { e0.max(base) };
        }
        let line = |x: f32| e0 + slope * (x - x0);
        for (k, &i) in group.iter().enumerate() {
            self.cols[i].stem_end = line(xs[k]);
        }

        let dir = if up { 1.0 } else { -1.0 };
        let half = g::STEM / 2.0;
        let seg = |items: &mut Vec<Item>, xa: f32, xb: f32, level: u8| {
            let off = (level - 1) as f32 * (g::BEAM + g::BEAM_SPACING) * dir;
            let (ya, yb) = (line(xa) + off, line(xb) + off);
            items.push(Item::Polygon {
                points: vec![(xa, ya), (xb, yb), (xb, yb + g::BEAM * dir), (xa, ya + g::BEAM * dir)],
            });
        };
        seg(&mut self.items, xs[0] - half, xs[n - 1] + half, 1);
        for level in 2..=levels {
            let mut k = 0;
            while k < n {
                if flags[k] < level {
                    k += 1;
                    continue;
                }
                let start = k;
                while k + 1 < n && flags[k + 1] >= level {
                    k += 1;
                }
                if k > start {
                    seg(&mut self.items, xs[start] - half, xs[k] + half, level);
                } else {
                    // a single short note: a stub toward its neighbour
                    let stub = 1.1_f32.min(dx / (n as f32 - 1.0).max(1.0) * 0.6);
                    if start == 0 {
                        seg(&mut self.items, xs[start] - half, xs[start] + stub, level);
                    } else {
                        seg(&mut self.items, xs[start] - stub, xs[start] + half, level);
                    }
                }
                k += 1;
            }
        }
    }

    /// Stems, flags and the vertical extent of a note, once beams are known.
    fn finish_column(&mut self, i: usize) {
        let c = self.cols[i].clone();
        if !matches!(c.kind, Kind::Note { .. }) {
            if matches!(c.kind, Kind::Bar(_) | Kind::Key { .. } | Kind::Meter(_)) {
                let col = &mut self.cols[i];
                col.top = 0.0;
                col.bottom = 4.0;
            }
            return;
        }
        let up = c.stem_up();
        let top = c.head_top();
        let bottom = c.head_bottom();
        let (mut ext_top, mut ext_bottom) = (top - 0.5, bottom + 0.5);
        for h in c.heads() {
            if let Some(a) = h.acc {
                let (u, d) = g::extent(accidental_glyph(a));
                ext_top = ext_top.min(h.y - u);
                ext_bottom = ext_bottom.max(h.y + d);
            }
        }
        if c.has_stem() {
            let attach = |y: f32| c.heads().iter().find(|h| h.y == y).map(|h| h.attach).unwrap_or(g::STEM_ATTACH_Y);
            let start = if up { bottom - attach(bottom) } else { top + attach(top) };
            self.line(c.stem_x, start, c.stem_x, c.stem_end, g::STEM);
            let flags = c.value.flags();
            if flags > 0 && matches!(c.kind, Kind::Note { beam: None, .. }) {
                let glyph = if up {
                    g::FLAG_UP[flags.min(4) as usize - 1]
                } else {
                    g::FLAG_DOWN[flags.min(4) as usize - 1]
                };
                self.glyph(c.stem_x - g::STEM / 2.0, c.stem_end, glyph, 1.0);
            }
            if up {
                ext_top = ext_top.min(c.stem_end);
            } else {
                ext_bottom = ext_bottom.max(c.stem_end);
            }
            if let Some(h) = self.hits.iter_mut().rev().find(|h| h.span == c.span) {
                h.rect = h.rect.union(&Rect::new(c.stem_x - 0.3, start, c.stem_x + 0.3, c.stem_end));
            }
        }
        let col = &mut self.cols[i];
        col.top = ext_top;
        col.bottom = ext_bottom;
    }

    fn draw_ties(&mut self, carry: &mut Carry) {
        let notes: Vec<usize> = (0..self.cols.len())
            .filter(|&i| matches!(self.cols[i].kind, Kind::Note { .. }))
            .collect();

        // ties carried from the previous system end on the first note
        let incoming = std::mem::take(&mut carry.ties);
        if let Some(&first) = notes.first() {
            let c = self.cols[first].clone();
            for (d, upward) in incoming {
                if let Some(h) = c.heads().iter().find(|h| h.d == d) {
                    let x1 = c.x + h.dx - 0.15;
                    let x0 = (x1 - 3.0).max(self.preamble_end - 1.5);
                    self.curve(x0, x1, h.y, h.y, upward, 0.15);
                }
            }
        }

        for (k, &i) in notes.iter().enumerate() {
            let c = self.cols[i].clone();
            let heads = c.heads();
            let n = heads.len();
            let next = notes.get(k + 1).map(|&j| self.cols[j].clone());
            for (hi, h) in heads.iter().enumerate() {
                if !h.tie {
                    continue;
                }
                let upward = match c.role {
                    Some(up) => up,
                    None if n > 1 => hi * 2 >= n,
                    None => !c.stem_up(),
                };
                let dots = if c.value.dots > 0 { 0.3 + c.value.dots as f32 * 0.55 } else { 0.0 };
                let x0 = c.x + h.dx + h.w + 0.15 + dots;
                let gap = if n > 1 { -0.2 } else { 0.15 };
                match &next {
                    Some(nc) => {
                        if let Some(nh) = nc.heads().iter().find(|o| o.d == h.d) {
                            let x1 = nc.x + nh.dx - 0.15;
                            self.curve(x0, x1, h.y, nh.y, upward, gap);
                        }
                    }
                    None => {
                        let x1 = self.staff_end.max(x0 + 1.5);
                        self.curve(x0, x1, h.y, h.y, upward, gap);
                        carry.ties.push((h.d, upward));
                    }
                }
            }
        }
    }

    /// A tie-like curve between two noteheads (`upward` bulges up).
    fn curve(&mut self, x0: f32, x1: f32, y0: f32, y1: f32, upward: bool, gap: f32) {
        let dir = if upward { -1.0 } else { 1.0 };
        let off = (0.35 + gap) * dir;
        let len = (x1 - x0).max(0.5);
        let h = (0.35 + 0.08 * len).min(1.0) * dir;
        self.bezier((x0, y0 + off), (x1, y1 + off), h, 0.16);
    }

    /// A curve from `p0` to `p3` whose middle is `h` from the chord (negative = up).
    fn bezier(&mut self, p0: (f32, f32), p3: (f32, f32), h: f32, thickness: f32) {
        let k = h / 0.75;
        let dx = p3.0 - p0.0;
        let c1 = (p0.0 + dx * 0.2, p0.1 + (p3.1 - p0.1) * 0.2 + k);
        let c2 = (p0.0 + dx * 0.8, p0.1 + (p3.1 - p0.1) * 0.8 + k);
        self.items.push(Item::Curve { p0, c1, c2, p3, thickness });
    }

    fn draw_slurs(&mut self) {
        let slurs = std::mem::take(&mut self.slurs);
        for s in &slurs {
            let (a, b) = match (s.start, s.end) {
                (Some(a), Some(b)) => (Some(a), Some(b)),
                (Some(a), None) => (Some(a), None),
                (None, Some(b)) => (None, Some(b)),
                (None, None) => (None, None),
            };
            let start = a.map(|i| self.cols[i].clone());
            let end = b.map(|i| self.cols[i].clone());
            // below only when the ends both have stems up
            let ups = [&start, &end].iter().filter_map(|c| c.as_ref()).all(|c| c.stem_up() && c.has_stem());
            // a voice that shares its staff keeps its slurs on its own side
            let role = [&start, &end].iter().filter_map(|c| c.as_ref()).find_map(|c| c.role);
            let above = role.unwrap_or(!ups);
            let edge = |c: &Col| -> (f32, f32) {
                let y = if c.heads().is_empty() {
                    // a rest
                    if above { c.top - 0.3 } else { c.bottom + 0.3 }
                } else if above {
                    if c.stem_up() && c.has_stem() { c.stem_end - 0.3 } else { c.head_top() - 0.8 }
                } else if !c.stem_up() && c.has_stem() {
                    c.stem_end + 0.3
                } else {
                    c.head_bottom() + 0.8
                };
                (c.center(), y)
            };
            let p0 = match &start {
                Some(c) => edge(c),
                None => (self.preamble_end - 1.0, end.as_ref().map(|c| edge(c).1).unwrap_or(-1.0)),
            };
            let p3 = match &end {
                Some(c) => edge(c),
                None => (self.staff_end, p0.1),
            };
            if p3.0 <= p0.0 {
                continue;
            }
            let dir = if above { -1.0 } else { 1.0 };
            let len = p3.0 - p0.0;
            let mut h = (0.5 + 0.12 * len).clamp(0.7, 2.0);
            // clear the notes in between
            let lo = a.map(|i| i + 1).unwrap_or(0);
            let hi = b.unwrap_or(self.cols.len());
            for k in lo..hi {
                let c = &self.cols[k];
                if !c.is_timed() {
                    continue;
                }
                let t = ((c.center() - p0.0) / len).clamp(0.1, 0.9);
                let chord = p0.1 + (p3.1 - p0.1) * t;
                let need = if above { chord - (c.top - 0.5) } else { (c.bottom + 0.5) - chord };
                h = h.max(need / (4.0 * t * (1.0 - t)) * 0.75);
            }
            h = h.min(4.0);
            self.bezier(p0, p3, h * dir, 0.22);
            for k in lo.saturating_sub(1)..hi.min(self.cols.len()) {
                let c = &mut self.cols[k];
                let t = ((c.center() - p0.0) / len).clamp(0.0, 1.0);
                let y = p0.1 + (p3.1 - p0.1) * t + 4.0 * t * (1.0 - t) * h * dir;
                if above {
                    c.top = c.top.min(y - 0.3);
                } else {
                    c.bottom = c.bottom.max(y + 0.3);
                }
            }
        }
        self.slurs = slurs;
    }

    fn draw_tuplets(&mut self) {
        let tuplets = std::mem::take(&mut self.tuplets);
        for t in &tuplets {
            if t.cols.is_empty() {
                continue;
            }
            let first = self.cols[t.cols[0]].clone();
            let last = self.cols[*t.cols.last().unwrap()].clone();
            let beam = |c: &Col| match c.kind {
                Kind::Note { beam, .. } => beam,
                _ => None,
            };
            let beamed = beam(&first).is_some() && t.cols.iter().all(|&i| beam(&self.cols[i]) == beam(&first));
            let digits: Vec<char> = t.p.to_string().chars().map(|d| g::tuplet_digit(d.to_digit(10).unwrap_or(3))).collect();
            let s = 0.8;
            let w: f32 = digits.iter().map(|d| g::width(*d) * s).sum();
            let x0 = first.center();
            let x1 = last.center();
            let cx = (x0 + x1) / 2.0;
            let below = match first.role {
                Some(up) => !up,
                None => beamed && !first.stem_up(),
            };
            let y = if below {
                t.cols.iter().map(|&i| self.cols[i].bottom).fold(f32::MIN, f32::max) + 0.3 + 1.5 * s
            } else {
                t.cols.iter().map(|&i| self.cols[i].top).fold(f32::MAX, f32::min) - 0.4
            };
            let mut gx = cx - w / 2.0;
            for d in &digits {
                self.glyph(gx, y, *d, s);
                gx += g::width(*d) * s;
            }
            if !beamed {
                let by = y - 0.6;
                let (l0, l1) = (x0 - 0.6, x1 + 0.6);
                self.line(l0, by, cx - w / 2.0 - 0.3, by, g::BRACKET);
                self.line(cx + w / 2.0 + 0.3, by, l1, by, g::BRACKET);
                // the hooks point at the notes
                let (h0, h1) = if below {
                    (by + g::BRACKET / 2.0, by - 0.6)
                } else {
                    (by - g::BRACKET / 2.0, by + 0.6)
                };
                self.line(l0, h0, l0, h1, g::BRACKET);
                self.line(l1, h0, l1, h1, g::BRACKET);
            }
            for &i in &t.cols {
                let c = &mut self.cols[i];
                if below {
                    c.bottom = c.bottom.max(y + 0.2);
                } else {
                    c.top = c.top.min(y - 1.5 * s);
                }
            }
        }
        self.tuplets = tuplets;
    }

    fn draw_endings(&mut self) {
        let endings = std::mem::take(&mut self.endings);
        for (k, e) in endings.iter().enumerate() {
            let start_x = match e.bar {
                Some(b) => self.cols[b].x + self.cols[b].right * 0.5,
                None => self.cols.get(e.first).map(|c| c.x - 0.5).unwrap_or(self.staff_end),
            };
            let next_ending = endings.get(k + 1).map(|n| n.bar.unwrap_or(n.first));
            // the ending runs to the next repeat, double bar or ending
            let mut end: Option<(f32, bool)> = None;
            for i in e.first..self.cols.len() {
                if Some(i) == next_ending {
                    end = Some((self.cols[i].x, false));
                    break;
                }
                if let Kind::Bar(b) = &self.cols[i].kind
                    && (b.starts_with(':') || b.contains("||") || b.contains(']') || b.contains('[')) {
                        end = Some((self.cols[i].x + self.cols[i].right * 0.5, b.starts_with(':')));
                        break;
                    }
            }
            let (end_x, closed) = end.unwrap_or((self.staff_end, false));
            let lo = e.bar.unwrap_or(e.first);
            let top = (lo..self.cols.len())
                .take_while(|&i| self.cols[i].x <= end_x)
                .map(|i| self.cols[i].top)
                .fold(-1.0_f32, f32::min);
            let y = (top - 1.0).min(-2.0);
            let end_x = end_x - 0.3;
            if end_x <= start_x {
                continue;
            }
            self.line(start_x, y, end_x, y, g::BRACKET);
            self.line(start_x, y - g::BRACKET / 2.0, start_x, y + 1.8, g::BRACKET);
            if closed {
                self.line(end_x, y - g::BRACKET / 2.0, end_x, y + 1.8, g::BRACKET);
            }
            self.text(start_x + 0.4, y + 1.4, &format!("{}.", e.n), SMALL_TEXT, Anchor::Start, TextStyle::Normal, None);
            for i in lo..self.cols.len() {
                if self.cols[i].x > end_x {
                    break;
                }
                self.cols[i].top = self.cols[i].top.min(y - 0.3);
            }
        }
        self.endings = endings;
    }

    fn draw_texts(&mut self) {
        // chord symbols share one height across the line
        let has_chords = self
            .cols
            .iter()
            .any(|c| c.texts.iter().any(|t| matches!(t.kind, TextKind::Chord | TextKind::Annotation(Placement::Above))));
        if !has_chords {
            return;
        }
        let top = self
            .cols
            .iter()
            .map(|c| c.top)
            .fold(0.0_f32, f32::min);
        let base = (top - 0.6).min(-1.4);
        let bottom = self.cols.iter().map(|c| c.bottom).fold(4.0_f32, f32::max);
        for i in 0..self.cols.len() {
            let c = self.cols[i].clone();
            let mut above = base;
            for t in &c.texts {
                match t.kind {
                    TextKind::Chord | TextKind::Annotation(Placement::Above) => {
                        self.text(c.x, above, &t.text, CHORD_SIZE, Anchor::Start, TextStyle::Normal, Some(t.span));
                        above -= CHORD_SIZE * 1.1;
                    }
                    _ => {}
                }
            }
            self.cols[i].top = self.cols[i].top.min(above + CHORD_SIZE * 0.2);
            let _ = bottom;
        }
    }

    fn draw_decorations(&mut self, i: usize) {
        let c = self.cols[i].clone();
        let mut above = (c.top - 0.6).min(-0.8);
        let mut below = (c.bottom + 0.8).max(4.8);
        let cx = c.center();
        let up = c.stem_up() && c.has_stem();
        let is_note = matches!(c.kind, Kind::Note { .. });

        // annotations other than above (those are drawn with the chord symbols)
        for t in &c.texts {
            match t.kind {
                TextKind::Annotation(Placement::Below) => {
                    let y = below + SMALL_TEXT;
                    self.text(c.x, y, &t.text, SMALL_TEXT, Anchor::Start, TextStyle::Normal, Some(t.span));
                    below = y + 0.4;
                }
                TextKind::Annotation(Placement::Left) => {
                    let y = (c.head_top().min(2.5)) + 0.5;
                    self.text(c.x - c.left - 0.3, y, &t.text, SMALL_TEXT, Anchor::End, TextStyle::Normal, Some(t.span));
                }
                TextKind::Annotation(Placement::Right) => {
                    let y = (c.head_top().min(2.5)) + 0.5;
                    self.text(c.x + c.right + 0.3, y, &t.text, SMALL_TEXT, Anchor::Start, TextStyle::Normal, Some(t.span));
                }
                TextKind::Annotation(Placement::Free) => {
                    self.text(c.x, -1.0, &t.text, SMALL_TEXT, Anchor::Start, TextStyle::Normal, Some(t.span));
                }
                _ => {}
            }
        }

        // where the last mark beyond the stem ends
        let mut near_above: Option<f32> = None;
        let mut near_below: Option<f32> = None;
        for d in &c.decos {
            let name = decoration_name(d);
            // articulations sit by the notehead, on the side away from the stem
            if let Some((ga, gb)) = articulation(&name)
                && is_note {
                    // a voice that shares its staff keeps its marks on its own side
                    if let Some(upper) = c.role {
                        // a dot stands on the stem, anything else over the head
                        let dot = matches!(name.as_str(), "staccato" | ".");
                        let x = if dot && c.has_stem() { c.stem_x } else { cx };
                        if upper {
                            let y = near_above.unwrap_or(if c.has_stem() { c.stem_end } else { c.head_top() }) - 0.6;
                            self.glyph(x - g::width(ga) / 2.0, y + 0.15, ga, 1.0);
                            near_above = Some(y - 0.2);
                            above = above.min(y - 0.6);
                        } else {
                            let y = near_below.unwrap_or(if c.has_stem() { c.stem_end } else { c.head_bottom() }) + 0.6;
                            self.glyph(x - g::width(gb) / 2.0, y - 0.15, gb, 1.0);
                            near_below = Some(y + 0.2);
                            below = below.max(y + 0.6);
                        }
                        continue;
                    }
                    if up || !c.has_stem() && c.head_top() >= 2.0 {
                        let mut y = c.head_bottom() + 0.75;
                        if y.fract() == 0.0 && y < 4.5 {
                            y += 0.5;
                        }
                        self.glyph(cx - g::width(gb) / 2.0, y - 0.15, gb, 1.0);
                    } else {
                        let mut y = c.head_top() - 0.75;
                        if y.fract() == 0.0 && y > -0.5 {
                            y -= 0.5;
                        }
                        self.glyph(cx - g::width(ga) / 2.0, y + 0.15, ga, 1.0);
                    }
                    continue;
                }
            if name == "fermata" && c.role == Some(false) {
                self.glyph(cx - g::width(g::FERMATA_BELOW) / 2.0, below - 0.2, g::FERMATA_BELOW, 1.0);
                below += g::height(g::FERMATA_ABOVE) + 0.3;
                continue;
            }
            if let Some(glyph) = above_glyph(&name) {
                self.glyph(cx - g::width(glyph) / 2.0, above, glyph, 1.0);
                above -= g::height(glyph) + 0.3;
                continue;
            }
            if name == "roll" {
                let y = above - 0.2;
                self.bezier((cx - 0.75, y), (cx + 0.75, y), -0.45, 0.12);
                above -= 0.9;
                continue;
            }
            if let Some(letters) = dynamic(&name) {
                let y = below + 1.6;
                let mut x = c.x - 0.3;
                for glyph in letters {
                    self.glyph(x, y, glyph, 1.0);
                    x += g::width(glyph) * 0.85;
                }
                below = y + 0.8;
                continue;
            }
            if let Some(t) = text_decoration(&name) {
                self.text(cx, above, t, SMALL_TEXT, Anchor::Middle, TextStyle::Italic, None);
                above -= SMALL_TEXT + 0.2;
            }
        }
        let col = &mut self.cols[i];
        col.top = col.top.min(above);
        col.bottom = col.bottom.max(below);
    }

    /// How many verses the lyrics of this staff have.
    fn verses(&self) -> usize {
        self.cols.iter().flat_map(|c| &c.under).map(|s| s.verse + 1).max().unwrap_or(0)
    }

    /// How low the notes and marks reach, once they are drawn.
    fn low(&self) -> f32 {
        self.cols.iter().map(|c| c.bottom).fold(4.0_f32, f32::max)
    }

    /// The lyrics: a row of syllables for each verse, below everything else
    /// on the staff. A hyphen stands halfway to the next syllable of the word,
    /// and a line runs under the notes that hold a syllable.
    /// `bottom` is how low the staff reaches with all its voices, and
    /// `first_row` how many rows the voices before this one have taken.
    fn draw_lyrics(&mut self, bottom: f32, first_row: usize) {
        let verses = self.verses();
        if verses == 0 {
            return;
        }
        let first = bottom + LYRIC_GAP + LYRIC_SIZE * 0.75;
        let hyphen_width = crate::text_width("-", LYRIC_SIZE);
        for verse in 0..verses {
            let y = first + (first_row + verse) as f32 * LYRIC_SIZE * LYRIC_LEADING;
            // what the verse has under the notes, left to right: (middle of the note, syllable)
            let row: Vec<(f32, Syllable)> = self
                .cols
                .iter()
                .flat_map(|c| c.under.iter().filter(|s| s.verse == verse).map(move |s| (c.x + c.mid, s.clone())))
                .collect();
            let mut k = 0;
            while k < row.len() {
                // the notes after this one that hold its syllable
                let mut last = k;
                while last + 1 < row.len() && row[last + 1].1.sung == Sung::Hold {
                    last += 1;
                }
                let (cx, syllable) = &row[k];
                // a staff may start with notes that hold a syllable of the staff before
                let mut held = k;
                let mut line_from = cx - HEAD_W / 2.0;
                let mut goes_on = false;
                if let Sung::Text { text, hyphen } = &syllable.sung {
                    self.text(*cx, y, text, LYRIC_SIZE, Anchor::Middle, TextStyle::Normal, Some(syllable.span));
                    let end = cx + crate::text_width(text, LYRIC_SIZE) / 2.0;
                    held = k + 1;
                    line_from = end + 0.2;
                    goes_on = *hyphen;
                    if *hyphen {
                        let next = row[k + 1..].iter().find_map(|(x, s)| match &s.sung {
                            Sung::Text { text, .. } => Some(x - crate::text_width(text, LYRIC_SIZE) / 2.0),
                            Sung::Hold => None,
                        });
                        match next {
                            Some(start) if start - end >= hyphen_width + 0.2 => {
                                self.text((end + start) / 2.0, y, "-", LYRIC_SIZE, Anchor::Middle, TextStyle::Normal, None);
                            }
                            // no room: the syllables stand together
                            Some(_) => {}
                            // the word goes on in the next system
                            None => self.text(end + 0.2, y, "-", LYRIC_SIZE, Anchor::Start, TextStyle::Normal, None),
                        }
                    }
                }
                for (x, s) in &row[held..last + 1] {
                    self.hits.push(Hit {
                        rect: Rect::new(x - 0.7, y - LYRIC_SIZE * 0.75, x + 0.7, y + LYRIC_SIZE * 0.25),
                        span: s.span,
                    });
                }
                let line_to = row[last].0 + HEAD_W / 2.0;
                if !goes_on && last >= held && line_to - line_from > 0.4 {
                    self.line(line_from, y, line_to, y, LYRIC_LINE);
                }
                k = last + 1;
            }
        }
    }

    /// The vertical extent of everything drawn.
    fn extent(&self) -> (f32, f32) {
        let mut lo = -1.0_f32;
        let mut hi = 5.0_f32;
        for it in &self.items {
            let r = it.bounds();
            lo = lo.min(r.y0);
            hi = hi.max(r.y1);
        }
        (lo, hi)
    }
}

//------------------------------------------------------------------------------
// systems

/// Staves that sound together. Columns that start at the same time share a
/// moment, and with it an x position, whichever staff they are on.
struct System {
    /// One for each voice, and for each extra voice that `&` makes; those of
    /// one staff follow each other.
    staves: Vec<Staff>,
    /// Width of the staves, in staff spaces.
    width: f32,
}

impl System {
    fn new(width: f32) -> System {
        System { staves: vec![], width }
    }

    /// Pass 3: places the columns of every staff horizontally.
    fn space(&mut self) {
        // key signatures, time signatures and the music start at the same x
        // on every staff
        let key_x = self.staves.iter().map(|s| s.key_x).fold(0.0, f32::max);
        for s in self.staves.iter_mut() {
            let dx = key_x - s.key_x;
            s.key_x = key_x;
            s.meter_x = s.meter_x.map(|x| x + dx);
            s.preamble_end += dx;
        }
        let meter_x = self.staves.iter().filter_map(|s| s.meter_x).fold(0.0, f32::max);
        for s in self.staves.iter_mut() {
            if let Some(x) = s.meter_x {
                s.meter_x = Some(meter_x);
                s.preamble_end += meter_x - x;
            }
        }
        let preamble_end = self.staves.iter().map(|s| s.preamble_end).fold(0.0, f32::max);
        for s in self.staves.iter_mut() {
            s.preamble_end = preamble_end;
        }

        // moments, in the order they sound
        let mut keys: Vec<(Rational64, u32)> = self
            .staves
            .iter()
            .flat_map(|s| s.cols.iter().map(Col::key))
            .collect();
        keys.sort();
        keys.dedup();
        let n = keys.len();
        if n == 0 {
            for s in self.staves.iter_mut() {
                s.staff_end = preamble_end + 2.0;
            }
            return;
        }
        for s in self.staves.iter_mut() {
            for c in s.cols.iter_mut() {
                c.moment = keys.binary_search(&c.key()).unwrap_or(0);
            }
        }
        self.share();

        // What each moment needs: the room after an earlier moment of the same
        // staff, or, for the first column of a staff (`None`), its x.
        let mut needs: Vec<Vec<(Option<usize>, f32)>> = vec![vec![]; n];
        let mut timed = vec![false; n];
        for s in &self.staves {
            for (i, c) in s.cols.iter().enumerate() {
                timed[c.moment] |= c.is_timed();
                if i == 0 {
                    needs[c.moment].push((None, preamble_end + c.left - c.offset));
                } else {
                    let prev = s.cols[i - 1].moment;
                    if prev < c.moment {
                        needs[c.moment].push((Some(prev), s.gap_after(i - 1)));
                    }
                }
            }
        }

        // syllables need room from one to the next, whatever stands between them
        for s in &self.staves {
            let mut before: Option<&Col> = None;
            for c in s.cols.iter().filter(|c| !c.under.is_empty()) {
                if let Some(b) = before
                    && b.moment < c.moment
                {
                    needs[c.moment].push((Some(b.moment), b.lyric_right + LYRIC_PAD + c.lyric_left));
                }
                before = Some(c);
            }
        }

        // a note that sounds on while other voices move gives each moment on
        // its way a share of its room
        let seconds = |t: Rational64| *t.numer() as f64 / *t.denom() as f64;
        for s in &self.staves {
            // the moments of the staff's own columns
            let mut own = vec![false; n];
            for c in &s.cols {
                own[c.moment] = true;
            }
            for c in s.cols.iter().filter(|c| c.is_timed()) {
                let end = c.onset + c.sounding();
                for m in c.moment + 1..n {
                    let (time, order) = keys[m];
                    if time >= end {
                        break;
                    }
                    if order == ORDER_TIMED && time > c.onset && !own[m] {
                        let share = (seconds(time) - seconds(c.onset)) / (seconds(end) - seconds(c.onset));
                        needs[m].push((Some(c.moment), c.space * share.clamp(0.0, 1.0) as f32));
                    }
                }
            }
        }

        // voices on one staff: what one has at a moment stands clear of what
        // another has at the next. For each staff and moment: how far the
        // notes and rests there reach to the left and to the right.
        let mut reach: std::collections::BTreeMap<(usize, usize), (f32, f32)> = Default::default();
        for s in &self.staves {
            if self.staves.iter().filter(|o| o.place == s.place).count() < 2 {
                continue;
            }
            for c in s.cols.iter().filter(|c| seen(c)) {
                let r = reach.entry((s.place, c.moment)).or_insert((f32::MIN, f32::MIN));
                r.0 = r.0.max(c.left - c.offset);
                r.1 = r.1.max(c.offset + c.right);
            }
        }
        let mut before: Option<(usize, usize, f32)> = None;
        for (&(place, moment), &(left, right)) in &reach {
            if let Some((p, m, r)) = before
                && p == place
            {
                needs[moment].push((Some(m), r + 0.4 + left));
            }
            before = Some((place, moment, right));
        }

        // natural gaps: gaps[m] is the distance from moment m to the next one,
        // the largest that any staff asks for
        let first_x = needs[0].iter().map(|&(_, x)| x).fold(0.0, f32::max);
        let mut gaps = vec![0.0_f32; n];
        let mut xs = vec![first_x; n];
        for m in 1..n {
            let mut gap = 0.0_f32;
            for &(from, need) in &needs[m] {
                gap = gap.max(match from {
                    Some(a) if a + 1 == m => need,
                    Some(a) => xs[a] + need - xs[m - 1],
                    None => need - xs[m - 1],
                });
            }
            gaps[m - 1] = gap;
            xs[m] = xs[m - 1] + gap;
        }
        // only the space after notes and rests may stretch
        let stretch: Vec<f32> = (0..n).map(|m| if timed[m] { gaps[m] } else { 0.0 }).collect();

        let place = |staves: &mut [Staff], gaps: &[f32]| {
            let mut xs = Vec::with_capacity(gaps.len());
            let mut x = first_x;
            for gap in gaps {
                xs.push(x);
                x += gap;
            }
            for c in staves.iter_mut().flat_map(|s| s.cols.iter_mut()) {
                c.x = xs[c.moment] + c.offset;
            }
        };
        place(&mut self.staves, &gaps);

        // stretch to the full width unless the line is short
        let ends = |staves: &[Staff]| -> f32 {
            staves
                .iter()
                .filter_map(|s| s.cols.last())
                .map(|c| c.x + c.right)
                .fold(f32::MIN, f32::max)
        };
        let lyrics_end = self
            .staves
            .iter()
            .flat_map(|s| &s.cols)
            .filter(|c| !c.under.is_empty())
            .map(|c| c.x + c.lyric_right)
            .fold(f32::MIN, f32::max);
        let natural = ends(&self.staves).max(lyrics_end);
        let extra = self.width - natural;
        let total: f32 = stretch.iter().sum();
        let mut staff_end = natural + 1.0;
        if total > 0.0 && (natural > 0.65 * self.width || extra < 0.0) {
            // never squeeze a gap below half its natural size
            let k = (extra / total).max(-0.5);
            for (gap, s) in gaps.iter_mut().zip(&stretch) {
                *gap += s * k;
            }
            place(&mut self.staves, &gaps);
            staff_end = self.width;
        }
        // a final bar line closes the staves
        let closed = self
            .staves
            .iter()
            .filter(|s| !s.overlay)
            .filter_map(|s| s.cols.last())
            .all(|c| matches!(c.kind, Kind::Bar(_)));
        if closed {
            staff_end = ends(&self.staves);
        }
        for s in self.staves.iter_mut() {
            s.staff_end = staff_end;
        }
    }

    /// The lines of the bar line that ends the system, as (x, thickness);
    /// none unless every staff with music ends with a bar line.
    fn end_bar(&self) -> Vec<(f32, f32)> {
        let ends: Vec<&Col> = self
            .staves
            .iter()
            .filter(|s| !s.overlay)
            .filter_map(|s| s.cols.last())
            .collect();
        let last = ends
            .iter()
            .copied()
            .max_by(|a, b| (a.x + a.right).total_cmp(&(b.x + b.right)));
        match last {
            Some(last) if ends.iter().all(|c| matches!(c.kind, Kind::Bar(_))) => bar_lines(last),
            _ => vec![],
        }
    }

    /// The lines, as (x, thickness), of the bar lines that the staff at
    /// `place` and the staff below it have at the same time.
    fn shared_bars(&self, place: usize) -> Vec<(f32, f32)> {
        let bars = |place: usize| {
            self.staves
                .iter()
                .filter(move |s| s.place == place)
                .flat_map(|s| &s.cols)
                .filter(|c| matches!(c.kind, Kind::Bar(_)) && !c.hidden)
        };
        let below: Vec<usize> = bars(place + 1).map(|c| c.moment).collect();
        bars(place)
            .filter(|c| below.contains(&c.moment))
            .flat_map(bar_lines)
            .collect()
    }

    /// Voices on one staff make room for each other wherever they have
    /// something at the same time: see `meet`.
    fn share(&mut self) {
        for i in 1..self.staves.len() {
            let (before, rest) = self.staves.split_at_mut(i);
            let second = &mut rest[0];
            let place = second.place;
            for first in before.iter_mut().filter(|s| s.place == place) {
                for b in second.cols.iter_mut() {
                    if let Some(a) = first.cols.iter_mut().find(|a| a.moment == b.moment) {
                        meet(a, b);
                    }
                }
            }
        }
    }
}

//------------------------------------------------------------------------------
// helpers

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum BarPart {
    Thin,
    Thick,
    Dots,
}

impl BarPart {
    fn width(self) -> f32 {
        match self {
            BarPart::Thin => g::THIN_BARLINE + g::BARLINE_SEPARATION,
            BarPart::Thick => g::THICK_BARLINE + g::BARLINE_SEPARATION,
            BarPart::Dots => g::width(g::REPEAT_DOT) + g::REPEAT_DOT_SEPARATION,
        }
    }

    fn separation(self) -> f32 {
        match self {
            BarPart::Thin | BarPart::Thick => g::BARLINE_SEPARATION,
            BarPart::Dots => g::REPEAT_DOT_SEPARATION,
        }
    }
}

/// Two columns of different voices at the same time on one staff; `a` is the
/// earlier voice's.
///
/// - A bar line, key or time signature that both have is drawn once.
/// - The same rest in both is one rest, where a rest stands on a staff of
///   one voice.
/// - Any other rest moves to its voice's side of the staff when the other
///   voice has something to see.
/// - Noteheads that would touch or cover each other stand side by side.
fn meet(a: &mut Col, b: &mut Col) {
    match (&a.kind, &b.kind) {
        (Kind::Bar(_), Kind::Bar(_)) | (Kind::Key { .. }, Kind::Key { .. }) | (Kind::Meter(_), Kind::Meter(_)) => {
            b.hidden = true;
            return;
        }
        (Kind::Rest { visible: true, measure: ma }, Kind::Rest { visible: true, measure: mb })
            if !a.hidden && a.value == b.value && ma == mb =>
        {
            b.hidden = true;
            a.rest_shift = 0.0;
            return;
        }
        _ => {}
    }
    if !seen(a) || !seen(b) {
        return;
    }
    a.rest_shift = rest_shift(a, b);
    b.rest_shift = rest_shift(b, a);
    if matches!((&a.kind, &b.kind), (Kind::Note { .. }, Kind::Note { .. })) {
        collide(a, b);
    }
}

/// A note, or a rest that is drawn.
fn seen(c: &Col) -> bool {
    match c.kind {
        Kind::Note { .. } => true,
        Kind::Rest { visible, .. } => visible && !c.hidden,
        _ => false,
    }
}

/// How far the rest `c` moves down when `other`, of another voice, stands on
/// the staff at the same time: to its voice's side of the staff, and then on
/// by whole spaces until it is clear of the other voice's notes. That way
/// whole and half rests stay on a line.
fn rest_shift(c: &Col, other: &Col) -> f32 {
    let (Kind::Rest { .. }, Some(upper)) = (&c.kind, c.role) else {
        return c.rest_shift;
    };
    let glyph = c.value.rest();
    let whole = glyph == g::REST_WHOLE;
    let y = if whole { 1.0 } else { 2.0 };
    let (up, down) = g::extent(glyph);
    let mut shift: f32 = match (upper, whole) {
        (true, true) => -1.0,
        (true, false) => -2.0,
        (false, _) => 2.0,
    };
    if matches!(other.kind, Kind::Note { .. }) {
        for _ in 0..8 {
            let clear = if upper {
                y + shift + down <= other.head_top() - 0.75
            } else {
                y + shift - up >= other.head_bottom() + 0.75
            };
            if clear {
                break;
            }
            shift += if upper { -1.0 } else { 1.0 };
        }
    }
    // with more than two voices: as far as any of them asks
    if c.rest_shift == 0.0 {
        shift
    } else if upper {
        shift.min(c.rest_shift)
    } else {
        shift.max(c.rest_shift)
    }
}

/// The notes of two voices at the same time on one staff. One of them moves
/// to the right when a notehead of it would touch or cover one of the other:
///
/// - a second apart, the voice with stems down below: its heads move until
///   the stems of both stand in one line, each voice on its side of them
/// - the same pitch: one head serves both voices when it looks the same in
///   both and the stems go opposite ways; if not, as for a second
/// - a second apart with the voices crossed: the voice with stems up moves,
///   so that the stems stand on the outside
/// - stems that go the same way: the second voice stands clear of the first
///
/// The accidentals of the voice that moves stand to the left of both voices,
/// and the dots of the voice that stays to the right of both.
fn collide(a: &mut Col, b: &mut Col) {
    let (up, down) = if !a.stem_up() && b.stem_up() { (b, a) } else { (a, b) };
    let opposite = up.stem_up() != down.stem_up();
    let right_of = |c: &Col| c.heads().iter().map(|h| h.dx + h.w).fold(0.0_f32, f32::max);
    let left_of = |c: &Col| c.heads().iter().map(|h| h.dx).fold(0.0_f32, f32::min);
    // how far the voice with stems down has to move, and how far the other
    let mut shift = 0.0_f32;
    let mut crossed = 0.0_f32;
    for u in up.heads() {
        for d in down.heads() {
            let clear = u.dx + u.w + 0.15 - d.dx;
            let in_line = u.dx + u.w - g::STEM - d.dx;
            match d.d - u.d {
                _ if !opposite && (d.d - u.d).abs() <= 1 => shift = shift.max(clear),
                0 => {
                    let same = up.value.notehead() == down.value.notehead()
                        && up.value.dots == down.value.dots
                        && u.raised == d.raised;
                    if !same {
                        shift = shift.max(in_line);
                    }
                }
                -1 => shift = shift.max(in_line),
                1 => crossed = crossed.max(d.dx + d.w + 0.15 - u.dx),
                _ => {}
            }
        }
    }
    // chords that ask for both: the second voice stands clear of all of the first
    if shift > 0.0 && crossed > 0.0 {
        shift = shift.max(right_of(up) + 0.15 - left_of(down));
        crossed = 0.0;
    }
    let (fixed, moved, shift) = if crossed > 0.0 { (down, up, crossed) } else { (up, down, shift) };

    // accidentals of both voices within a sixth would stand on each other,
    // and so would an accidental and a head on the far side of a stem
    let near = |h: &Head| moved.heads().iter().any(|m| m.acc.is_some() && (m.d - h.d).abs() < 6);
    let block = fixed
        .heads()
        .iter()
        .filter(|h| near(h))
        .map(|h| if h.acc.is_some() { -h.acc_x } else { -h.dx })
        .fold(0.0_f32, f32::max);
    if shift == 0.0 && block == 0.0 {
        return;
    }
    if shift > 0.0 && fixed.value.dots > 0 {
        let dots_x = (shift + right_of(moved) - right_of(fixed)).max(0.0);
        fixed.dots_x = fixed.dots_x.max(dots_x);
        fixed.right += dots_x;
    }
    moved.offset = moved.offset.max(shift);
    let accidentals = moved.heads().iter().any(|h| h.acc.is_some());
    if accidentals || moved.grace.is_some() {
        moved.left += shift + block;
    }
    if let Kind::Note { heads, .. } = &mut moved.kind {
        for h in heads.iter_mut().filter(|h| h.acc.is_some()) {
            h.acc_x -= shift + block;
        }
    }
}

/// The lines of a bar line, as (x, thickness); none for any other column.
fn bar_lines(c: &Col) -> Vec<(f32, f32)> {
    let Kind::Bar(bar) = &c.kind else {
        return vec![];
    };
    let mut lines = vec![];
    let mut x = c.x;
    for p in bar_parts(bar) {
        match p {
            BarPart::Thin => lines.push((x + g::THIN_BARLINE / 2.0, g::THIN_BARLINE)),
            BarPart::Thick => lines.push((x + g::THICK_BARLINE / 2.0, g::THICK_BARLINE)),
            BarPart::Dots => {}
        }
        x += p.width();
    }
    lines
}

/// A tall brace is a little wider than a short one.
fn brace_width(height: f32) -> f32 {
    (0.8 + 0.045 * height).min(BRACE_WIDTH)
}

/// A brace from `top` to `bottom` with its two ends at `x` and its tip to the
/// left of them: an upper and a lower half, each a filled outline. A brace
/// has to grow with the distance between its staves without getting fatter,
/// which a glyph of the music font cannot do.
fn brace(x: f32, top: f32, bottom: f32) -> Vec<Item> {
    const STEPS: usize = 24;
    let width = brace_width(bottom - top);
    let half = (bottom - top) / 2.0;
    let middle = (top + bottom) / 2.0;
    // the middle line of a half, from the tip at (0, 0) to its end at (1, 1):
    // out of the tip to the right, up along the spine, and to the right again
    let spine = |t: f32| {
        let s = 1.0 - t;
        let u = 3.0 * s * s * t * 1.25 - 3.0 * s * t * t * 0.35 + t * t * t;
        let v = 3.0 * s * s * t * 0.22 + 3.0 * s * t * t * 0.82 + t * t * t;
        (u, v)
    };
    [-1.0_f32, 1.0]
        .iter()
        .map(|side| {
            let at = |t: f32| {
                let (u, v) = spine(t);
                (x - width + u * width, middle + side * v * half)
            };
            let mut outline = vec![];
            let mut back = vec![];
            for k in 0..=STEPS {
                let t = k as f32 / STEPS as f32;
                let (px, py) = at(t);
                let (ax, ay) = at((t - 0.01).max(0.0));
                let (bx, by) = at((t + 0.01).min(1.0));
                let (dx, dy) = (bx - ax, by - ay);
                let length = (dx * dx + dy * dy).sqrt().max(1e-4);
                // thin at the tip and at the end, full in between
                let thick = width * (0.06 + 0.34 * (std::f32::consts::PI * t).sin().max(0.0).powf(0.8)) / 2.0;
                let (nx, ny) = (-dy / length * thick, dx / length * thick);
                outline.push((px + nx, py + ny));
                back.push((px - nx, py - ny));
            }
            outline.extend(back.into_iter().rev());
            Item::Polygon { points: outline }
        })
        .collect()
}

/// The parts of a bar line, left to right.
fn bar_parts(b: &str) -> Vec<BarPart> {
    use BarPart::*;
    let left = b.len() - b.trim_start_matches(':').len();
    let rest = b.trim_start_matches(':');
    let right = rest.len() - rest.trim_end_matches(':').len();
    let middle = rest.trim_end_matches(':');
    let mut parts = vec![];
    if left > 0 {
        parts.push(Dots);
    }
    if middle.is_empty() {
        // "::"
        parts.extend([Thin, Thick, Thin]);
        parts.push(Dots);
        return parts;
    }
    if middle.contains('[') || middle.contains(']') {
        for c in middle.chars() {
            parts.push(if c == '|' { Thin } else { Thick });
        }
    } else if left > 0 && right > 0 {
        parts.extend([Thin, Thick, Thin]);
    } else if left > 0 {
        parts.extend([Thin, Thick]);
    } else if right > 0 {
        parts.extend([Thick, Thin]);
    } else {
        let n = middle.matches('|').count().clamp(1, 2);
        for _ in 0..n {
            parts.push(Thin);
        }
    }
    if right > 0 {
        parts.push(Dots);
    }
    parts
}

fn meter_glyphs(s: &str) -> Vec<char> {
    s.chars()
        .map(|c| match c.to_digit(10) {
            Some(d) => g::time_sig_digit(d),
            None => g::TIME_SIG_PLUS,
        })
        .collect()
}

fn meter_width(m: &Meter) -> f32 {
    match m {
        Meter::None => 0.0,
        Meter::Common => g::width(g::TIME_SIG_COMMON),
        Meter::Cut => g::width(g::TIME_SIG_CUT),
        Meter::Fraction { num, den } => {
            let top: String = num.iter().map(|n| n.to_string()).collect::<Vec<_>>().join("+");
            let w = |s: &str| meter_glyphs(s).iter().map(|c| g::width(*c)).sum::<f32>();
            w(&top).max(w(&den.to_string()))
        }
    }
}

fn decoration_name(d: &Decoration) -> String {
    match d {
        Decoration::Staccato => "staccato".into(),
        Decoration::Roll => "roll".into(),
        Decoration::Fermata => "fermata".into(),
        Decoration::Accent => "accent".into(),
        Decoration::LowerMordent => "lowermordent".into(),
        Decoration::Coda => "coda".into(),
        Decoration::UpperMordent => "uppermordent".into(),
        Decoration::Segno => "segno".into(),
        Decoration::Trill => "trill".into(),
        Decoration::UpBow => "upbow".into(),
        Decoration::DownBow => "downbow".into(),
        Decoration::Unresolved(s) => s.to_ascii_lowercase(),
    }
}

/// (above, below) glyphs for marks placed at the notehead.
fn articulation(name: &str) -> Option<(char, char)> {
    match name {
        "staccato" | "." => Some((g::STACCATO_ABOVE, g::STACCATO_BELOW)),
        "accent" | ">" | "emphasis" => Some((g::ACCENT_ABOVE, g::ACCENT_BELOW)),
        "tenuto" => Some((g::TENUTO_ABOVE, g::TENUTO_BELOW)),
        _ => None,
    }
}

fn above_glyph(name: &str) -> Option<char> {
    Some(match name {
        "fermata" => g::FERMATA_ABOVE,
        "trill" | "tr" => g::TRILL,
        "lowermordent" | "mordent" => g::MORDENT,
        "uppermordent" | "pralltriller" => g::SHORT_TRILL,
        "turn" => g::TURN,
        "segno" => g::SEGNO,
        "coda" => g::CODA,
        "upbow" => g::UP_BOW,
        "downbow" => g::DOWN_BOW,
        "marcato" | "^" => g::MARCATO_ABOVE,
        "breath" => g::BREATH_MARK,
        _ => return None,
    })
}

fn dynamic(name: &str) -> Option<Vec<char>> {
    if name.is_empty() || name.len() > 4 {
        return None;
    }
    name.chars()
        .map(|c| match c {
            'p' => Some(g::DYN_P),
            'm' => Some(g::DYN_M),
            'f' => Some(g::DYN_F),
            'r' => Some(g::DYN_R),
            's' => Some(g::DYN_S),
            'z' => Some(g::DYN_Z),
            _ => None,
        })
        .collect()
}

fn text_decoration(name: &str) -> Option<&'static str> {
    Some(match name {
        "d.c." => "D.C.",
        "d.s." => "D.S.",
        "fine" => "Fine",
        "dacapo" => "Da Capo",
        "dacoda" => "Da Coda",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(src: &str) -> Engraving {
        engrave(&abc::parse(src), &Options::default())
    }

    #[test]
    fn bar_lines() {
        use BarPart::*;
        assert_eq!(bar_parts("|"), vec![Thin]);
        assert_eq!(bar_parts("||"), vec![Thin, Thin]);
        assert_eq!(bar_parts("|]"), vec![Thin, Thick]);
        assert_eq!(bar_parts("[|"), vec![Thick, Thin]);
        assert_eq!(bar_parts(":|"), vec![Dots, Thin, Thick]);
        assert_eq!(bar_parts("|:"), vec![Thick, Thin, Dots]);
        assert_eq!(bar_parts("::"), vec![Dots, Thin, Thick, Thin, Dots]);
        assert_eq!(bar_parts(":|:"), vec![Dots, Thin, Thick, Thin, Dots]);
    }

    #[test]
    fn empty_document() {
        let s = score("");
        assert!(s.items.is_empty());
    }

    #[test]
    fn every_note_gets_a_hit_box() {
        let src = "X:1\nT:T\nK:D\nA2 B|c>d (3efg|\n";
        let s = score(src);
        for (i, c) in src.char_indices().skip(src.find("A2").unwrap()) {
            if c.is_ascii_alphabetic() && c != 'K' {
                assert!(s.hits_at_source(i).next().is_some(), "no hit for {:?} at {}", c, i);
            }
        }
    }

    #[test]
    fn beams_and_flags() {
        // two beamed eighths, then a lone eighth with a flag
        let s = score("X:1\nT:T\nL:1/8\nK:C\nAB C\n");
        let beams = s.items.iter().filter(|i| matches!(i, Item::Polygon { .. })).count();
        let flags = s
            .items
            .iter()
            .filter(|i| matches!(i, Item::Glyph { glyph, .. } if g::FLAG_UP.contains(glyph) || g::FLAG_DOWN.contains(glyph)))
            .count();
        assert_eq!(beams, 1);
        assert_eq!(flags, 1);
    }

    #[test]
    fn stem_directions() {
        let s = score("X:1\nT:T\nL:1/4\nK:C\nC c'\n");
        // the low C has its stem up (end above the head), the high c' down
        let stems: Vec<(f32, f32)> = s
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Line { y1, y2, width, .. } if (*width - g::STEM).abs() < 1e-6 => Some((*y1, *y2)),
                _ => None,
            })
            .collect();
        assert_eq!(stems.len(), 2);
        assert!(stems[0].1 < stems[0].0);
        assert!(stems[1].1 > stems[1].0);
    }

    #[test]
    fn broken_rhythm_makes_a_dot() {
        let s = score("X:1\nT:T\nL:1/8\nK:C\nA>B\n");
        let dots = s
            .items
            .iter()
            .filter(|i| matches!(i, Item::Glyph { glyph, .. } if *glyph == g::AUGMENTATION_DOT))
            .count();
        assert_eq!(dots, 1);
    }

    #[test]
    fn sharp_notation() {
        let shapes = |src: &str| {
            score(src)
                .items
                .iter()
                .filter(|i| matches!(i, Item::Shape { .. }))
                .count()
        };
        assert_eq!(shapes("X:1\nT:T\nL:1/4\nK:C\nI K N Q W i k n q w|\n"), 10);
        assert_eq!(shapes("X:1\nT:T\nL:1/4\nK:C\n[DNA]2 W4|{n}d\n"), 3);

        // no key signature in a sharp-notation tune, even with K:D
        let sharps = |src: &str| {
            score(src)
                .items
                .iter()
                .filter(|i| matches!(i, Item::Glyph { glyph, .. } if *glyph == g::SHARP))
                .count()
        };
        assert_eq!(sharps("X:1\nT:T\nK:D\nDENG ABid|\n"), 0);
        assert_eq!(sharps("X:1\nT:T\nK:D\nDEFG ABcd|\n"), 2);
    }

    #[test]
    fn raised_stems_meet_the_head() {
        // up stem: right edge of the stem at 1.275 staff spaces from the head's left edge
        let s = score("X:1\nT:T\nL:1/4\nK:C\nI\n");
        let head_x = s.items.iter().find_map(|i| match i {
            Item::Shape { x, .. } => Some(*x),
            _ => None,
        });
        let stem_x = s.items.iter().find_map(|i| match i {
            Item::Line { x1, width, .. } if (*width - g::STEM).abs() < 1e-6 => Some(*x1),
            _ => None,
        });
        let (h, st) = (head_x.unwrap(), stem_x.unwrap());
        assert!((st + g::STEM / 2.0 - h - crate::Shape::RaisedBlack.stem_up_x()).abs() < 1e-4);
    }

    #[test]
    fn lines_are_stacked() {
        let s = score("X:1\nT:T\nK:C\nABcd|\nABcd|\n");
        let staff_tops: Vec<f32> = s
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Line { y1, y2, width, .. } if *width == g::STAFF_LINE && y1 == y2 => Some(*y1),
                _ => None,
            })
            .collect();
        assert_eq!(staff_tops.len(), 10);
        assert!(staff_tops[5] > staff_tops[4] + 4.0);
    }

    /// One music line in 4/4 with L:1/8, read and measured.
    fn staff(music: &str) -> Staff {
        let doc = abc::parse(&format!("X:1\nT:T\nK:C\n{}\n", music));
        // an empty line has no music
        let symbols: Vec<&Symbol> = doc.tunes[0].music_lines().flat_map(|l| &l.symbols).collect();
        let mut state = State {
            sharp_notation: false,
            fifths: 0,
            clef: Clef::Treble,
            meter: parse_meter("4/4"),
            unit: Rational32::new(1, 8),
            shift: 0,
            stem: None,
        };
        let mut s = Staff::new(state.clone());
        s.read(&symbols, &mut state, &mut Carry::default());
        s.prepare(false);
        s
    }

    fn onsets(music: &str) -> Vec<Rational64> {
        staff(music).cols.iter().map(|c| c.onset).collect()
    }

    fn t(n: i64, d: i64) -> Rational64 {
        Rational64::new(n, d)
    }

    #[test]
    fn onsets_add_up() {
        assert_eq!(onsets("A2 B|c"), vec![t(0, 1), t(1, 4), t(3, 8), t(3, 8)]);
        // broken rhythm moves the second note
        assert_eq!(onsets("A>B c"), vec![t(0, 1), t(3, 16), t(1, 4)]);
        assert_eq!(onsets("A<B c"), vec![t(0, 1), t(1, 16), t(1, 4)]);
    }

    #[test]
    fn tuplets_take_their_sounding_time() {
        // three eighths in the time of two
        assert_eq!(onsets("(3ABc d"), vec![t(0, 1), t(1, 12), t(1, 6), t(1, 4)]);
        // two in the time of three, and an explicit (5:4
        assert_eq!(onsets("(2AB c"), vec![t(0, 1), t(3, 16), t(3, 8)]);
        assert_eq!(onsets("(5:4ABcde f"), vec![t(0, 1), t(1, 10), t(1, 5), t(3, 10), t(2, 5), t(1, 2)]);
        // the written value is still what is drawn
        assert!(staff("(3ABc").cols.iter().all(|c| c.value == NoteValue { log: -3, dots: 0 }));
    }

    #[test]
    fn bar_rests_last_all_their_bars() {
        assert_eq!(onsets("Z|A"), vec![t(0, 1), t(1, 1), t(1, 1)]);
        assert_eq!(onsets("Z4|A"), vec![t(0, 1), t(4, 1), t(4, 1)]);
    }

    #[test]
    fn columns_at_one_time_keep_their_order() {
        let s = staff("A|[K:G][M:3/4]B|| |c");
        let order: Vec<u32> = s.cols.iter().map(|c| c.order).collect();
        assert_eq!(
            order,
            vec![ORDER_TIMED, ORDER_BAR, ORDER_KEY, ORDER_METER, ORDER_TIMED, ORDER_BAR, ORDER_BAR + 1, ORDER_TIMED]
        );
        // keys never go backwards within a staff
        assert!(s.cols.windows(2).all(|w| w[0].key() < w[1].key()));
    }

    fn spaced(music: &[&str]) -> System {
        let mut sys = System::new(114.0);
        sys.staves = music.iter().map(|m| staff(m)).collect();
        sys.space();
        sys
    }

    #[test]
    fn one_staff_is_spaced_by_its_own_gaps() {
        let sys = spaced(&["A2 B|c"]);
        let s = &sys.staves[0];
        assert_eq!(s.cols[0].x, s.preamble_end + s.cols[0].left);
        for i in 0..s.cols.len() - 1 {
            assert_eq!(s.cols[i + 1].x, s.cols[i].x + s.gap_after(i));
        }
    }

    #[test]
    fn staves_share_moments() {
        let sys = spaced(&["C4 D4|", "cdef gabc'|"]);
        let (a, b) = (&sys.staves[0], &sys.staves[1]);
        assert_eq!(a.preamble_end, b.preamble_end);
        assert_eq!(a.cols[0].x, b.cols[0].x);
        assert_eq!(a.cols[1].x, b.cols[4].x);
        assert_eq!(a.cols[2].x, b.cols[8].x);
        assert_eq!(a.staff_end, b.staff_end);
        // the eighths set the pace: every one keeps its own room
        for i in 0..7 {
            assert!(b.cols[i + 1].x >= b.cols[i].x + b.gap_after(i) - 1e-4);
        }
        // and neither staff is tighter than it would be alone
        assert!(a.cols[1].x >= a.cols[0].x + a.gap_after(0) - 1e-4);
    }

    #[test]
    fn a_triplet_lines_up_with_the_beat() {
        let sys = spaced(&["(3cde f2|", "C2 D2|"]);
        let (a, b) = (&sys.staves[0], &sys.staves[1]);
        assert_eq!(a.cols[3].x, b.cols[1].x);
        assert_eq!(a.cols[4].x, b.cols[2].x);
        // the triplet's second and third notes have moments of their own
        assert!(a.cols[1].x > b.cols[0].x && a.cols[2].x < b.cols[1].x);
    }

    #[test]
    fn a_staff_without_music_keeps_the_system_whole() {
        let sys = spaced(&["", "C2 D2|"]);
        assert_eq!(sys.staves[0].staff_end, sys.staves[1].staff_end);
        let empty = spaced(&[""]);
        assert_eq!(empty.staves[0].staff_end, empty.staves[0].preamble_end + 2.0);
    }

    /// The y of the top line of every staff, down the page.
    fn staff_tops(s: &Engraving) -> Vec<f32> {
        let lines: Vec<f32> = s
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Line { y1, y2, width, .. } if *width == g::STAFF_LINE && y1 == y2 => Some(*y1),
                _ => None,
            })
            .collect();
        lines.chunks(5).map(|c| c[0]).collect()
    }

    /// The lines that join the staves of a system: (top, bottom).
    fn joins(s: &Engraving) -> Vec<(f32, f32)> {
        s.items
            .iter()
            .filter_map(|i| match i {
                Item::Line { x1, y1, x2, y2, width } if *width == g::THIN_BARLINE && x1 == x2 && y2 - y1 > 5.0 => {
                    Some((*y1, *y2))
                }
                _ => None,
            })
            .collect()
    }

    /// The x and y of every notehead, in drawing order.
    fn noteheads(s: &Engraving) -> Vec<(f32, f32)> {
        s.items
            .iter()
            .filter_map(|i| match i {
                Item::Glyph { x, y, glyph, .. } if *glyph == g::NOTEHEAD_BLACK => Some((*x, *y)),
                _ => None,
            })
            .collect()
    }

    fn texts(s: &Engraving) -> Vec<&str> {
        s.items
            .iter()
            .filter_map(|i| match i {
                Item::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn one_voice_has_no_join() {
        let s = score("X:1\nT:T\nK:C\nABcd|\nABcd|\n");
        assert_eq!(staff_tops(&s).len(), 2);
        assert!(joins(&s).is_empty());
        assert!(s.diagnostics.is_empty());
    }

    #[test]
    fn two_voices_make_one_system() {
        let s = score("X:1\nT:T\nL:1/4\nV:1\nV:2 clef=bass\nK:C\nV:1\nc d e f|\nV:2\nC, D, E, F,|\n");
        let tops = staff_tops(&s);
        assert_eq!(tops.len(), 2);
        assert!(tops[1] >= tops[0] + 4.0 + STAFF_GAP);
        // a line on the left and the final bar line on the right join them, from the top line
        // of the first to the bottom line of the second
        let j = joins(&s);
        assert_eq!(j.len(), 2);
        for (top, bottom) in j {
            assert!((top - tops[0]).abs() < 0.1 && (bottom - (tops[1] + 4.0)).abs() < 0.1);
        }
        // each staff has its own clef
        let clefs = |c: char| s.items.iter().filter(|i| matches!(i, Item::Glyph { glyph, .. } if *glyph == c)).count();
        assert_eq!((clefs(g::G_CLEF), clefs(g::F_CLEF)), (1, 1));
        // notes that sound together stand above each other
        let heads = noteheads(&s);
        assert_eq!(heads.len(), 8);
        for k in 0..4 {
            assert_eq!(heads[k].0, heads[k + 4].0);
            assert!(heads[k + 4].1 > heads[k].1);
        }
        assert!(s.diagnostics.is_empty());
    }

    #[test]
    fn systems_take_one_line_from_each_voice() {
        // voice after voice, and line by line
        let blocks = score("X:1\nT:T\nK:C\nV:1\nABcd|\nefga|\nV:2\nABcd|\nefga|\n");
        let mixed = score("X:1\nT:T\nK:C\n[V:1]ABcd|\n[V:2]ABcd|\n[V:1]efga|\n[V:2]efga|\n");
        for s in [&blocks, &mixed] {
            assert_eq!(staff_tops(s).len(), 4);
            assert_eq!(joins(s).len(), 4);
            assert!(s.diagnostics.is_empty());
        }
        assert_eq!(staff_tops(&blocks), staff_tops(&mixed));
        assert_eq!(noteheads(&blocks), noteheads(&mixed));
    }

    #[test]
    fn a_voice_without_a_line_keeps_its_staff() {
        let s = score("X:1\nT:T\nK:C\nV:1\nABcd|\nefga|\nV:2\nABcd|\n");
        assert_eq!(staff_tops(&s).len(), 4);
        assert_eq!(noteheads(&s).len(), 12);
        assert!(s.diagnostics.is_empty());
    }

    #[test]
    fn voice_names_stand_before_the_staves() {
        let src = "X:1\nT:T\nV:S name=\"Soprano\" snm=S.\nV:A name=\"Alto\"\nK:C\n[V:S]ABcd|\n[V:A]ABcd|\n[V:S]efga|\n[V:A]efga|\n";
        let s = score(src);
        assert_eq!(texts(&s), vec!["T", "Soprano", "Alto", "S."]);
        // the staves start after the longest name, and further left when the names are short
        let starts: Vec<f32> = s
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Line { x1, y1, y2, width, .. } if *width == g::STAFF_LINE && y1 == y2 => Some(*x1),
                _ => None,
            })
            .collect();
        let margin = Options::default().margin;
        assert!(starts[0] > margin + crate::text_width("Soprano", LABEL_SIZE));
        assert_eq!(starts[0], starts[5]);
        assert!(starts[10] < starts[0] && starts[10] > margin);
        assert_eq!(starts[10], starts[15]);
        // a name leads to the V: field it was written in
        let name = src.find("name=\"Alto\"").unwrap();
        assert!(s.hits_at_source(name).next().is_some());
    }

    #[test]
    fn middle_says_which_pitch_sits_on_the_middle_line() {
        let y = |src: &str| {
            let s = score(src);
            noteheads(&s)[0].1 - staff_tops(&s)[0]
        };
        assert_eq!(y("X:1\nT:T\nL:1/4\nV:1 clef=bass\nK:C\nD,\n"), 2.0);
        assert_eq!(y("X:1\nT:T\nL:1/4\nV:1 clef=bass middle=d\nK:C\nd\n"), 2.0);
        assert_eq!(y("X:1\nT:T\nL:1/4\nV:1 clef=bass octave=-2\nK:C\nd\n"), 2.0);
        assert_eq!(y("X:1\nT:T\nL:1/4\nV:1 middle=B\nK:C\nB\n"), 2.0);
        // a clef written in the music stands on its own again
        assert_eq!(y("X:1\nT:T\nL:1/4\nV:1 clef=bass middle=d\nK:C\n[K:C clef=treble]B\n"), 2.0);
    }

    #[test]
    fn stems_follow_the_voice() {
        let stems = |src: &str| -> Vec<bool> {
            score(src)
                .items
                .iter()
                .filter_map(|i| match i {
                    Item::Line { y1, y2, width, .. } if (*width - g::STEM).abs() < 1e-6 => Some(y2 < y1),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(stems("X:1\nT:T\nL:1/4\nK:C\nC c'\n"), vec![true, false]);
        assert_eq!(stems("X:1\nT:T\nL:1/4\nV:1 stem=up\nK:C\nC c'\n"), vec![true, true]);
        assert_eq!(stems("X:1\nT:T\nL:1/4\nV:1 stem=down\nK:C\nC c'\n"), vec![false, false]);
    }

    #[test]
    fn key_signatures_line_up_under_different_clefs() {
        let s = score("X:1\nT:T\nM:3/4\nL:1/4\nV:1 clef=alto\nV:2\nK:G\n[V:1]C D E|\n[V:2]c d e|\n");
        let xs = |c: char| -> Vec<f32> {
            s.items
                .iter()
                .filter_map(|i| match i {
                    Item::Glyph { x, glyph, .. } if *glyph == c => Some(*x),
                    _ => None,
                })
                .collect()
        };
        let sharps = xs(g::SHARP);
        assert_eq!(sharps.len(), 2);
        assert_eq!(sharps[0], sharps[1]);
        let threes = xs(g::time_sig_digit(3));
        assert_eq!(threes.len(), 2);
        assert_eq!(threes[0], threes[1]);
    }

    #[test]
    fn body_fields_reach_the_right_voices() {
        let sharps = |src: &str| {
            score(src)
                .items
                .iter()
                .filter(|i| matches!(i, Item::Glyph { glyph, .. } if *glyph == g::SHARP))
                .count()
        };
        // before the body names a voice: both voices, on both staves of the second system
        assert_eq!(sharps("X:1\nT:T\nV:1\nV:2\nK:C\nK:G\n[V:1]ABcd|\n[V:2]ABcd|\n"), 2);
        // after: only the voice it is written in
        assert_eq!(sharps("X:1\nT:T\nK:C\nV:1\nABcd|\nK:G\nABcd|\nV:2\nABcd|\nABcd|\n"), 1);
    }

    #[test]
    fn lines_of_different_lengths_are_reported() {
        let src = "X:1\nT:T\nL:1/4\nK:C\n[V:1]c d e f|\n[V:2] C D E| \n[V:3]C D E F|\n";
        let s = score(src);
        assert_eq!(s.diagnostics.len(), 1);
        let d = &s.diagnostics[0];
        assert_eq!(&src[d.span.start..d.span.end], "C D E|");
        assert_eq!(d.message, "voice 2: this line is 3 units of L:1/4 long, the other voices have 4");
        // with two voices the longer line is taken to be right
        let s = score("X:1\nT:T\nL:1/8\nK:C\n[V:1]c3|\n[V:2]C4|\n");
        assert_eq!(s.diagnostics.len(), 1);
        assert!(s.diagnostics[0].message.starts_with("voice 1: this line is 3 units of L:1/8 long"));
    }

    #[test]
    fn ties_and_slurs_stay_in_their_voice() {
        // voice 1 ties over the line end, voice 2 does not: one tie ends on each staff of system 1,
        // and only the first staff of system 2 starts with one
        let s = score("X:1\nT:T\nL:1/4\nK:C\n[V:1]c d e f-|\n[V:2]C D E F|\n[V:1]f d e f|\n[V:2]C D E F|\n");
        let curves = s.items.iter().filter(|i| matches!(i, Item::Curve { .. })).count();
        assert_eq!(curves, 2);
        let tops = staff_tops(&s);
        let on_staff = |k: usize| {
            s.items
                .iter()
                .filter(|i| matches!(i, Item::Curve { p0, .. } if p0.1 > tops[k] - 3.0 && p0.1 < tops[k] + 7.0))
                .count()
        };
        assert_eq!((on_staff(0), on_staff(1), on_staff(2), on_staff(3)), (1, 0, 1, 0));
    }

    #[test]
    fn the_final_bar_line_closes_the_system() {
        // every part of the bar line runs through, also past a staff without music
        let lines = |src: &str| -> Vec<f32> {
            let s = score(src);
            let tops = staff_tops(&s);
            s.items
                .iter()
                .filter_map(|i| match i {
                    Item::Line { x1, y1, x2, y2, width }
                        if x1 == x2 && *y1 == tops[0] && *y2 == tops[tops.len() - 1] + 4.0 =>
                    {
                        Some(*width)
                    }
                    _ => None,
                })
                .collect()
        };
        assert_eq!(lines("X:1\nT:T\nK:C\n[V:1]ABcd|\n[V:2]ABcd|\n"), vec![g::THIN_BARLINE]);
        assert_eq!(lines("X:1\nT:T\nK:C\n[V:1]ABcd|]\n[V:2]ABcd|]\n"), vec![g::THIN_BARLINE, g::THICK_BARLINE]);
        assert_eq!(lines("X:1\nT:T\nK:C\nV:1\nV:2\nV:3\nK:C\n[V:1]ABcd:|\n[V:3]ABcd:|\n"), vec![g::THIN_BARLINE, g::THICK_BARLINE]);
        // music that does not end with a bar line stays open
        assert!(lines("X:1\nT:T\nK:C\n[V:1]ABcd\n[V:2]ABcd\n").is_empty());
        assert!(lines("X:1\nT:T\nK:C\n[V:1]ABcd|\n[V:2]ABcd\n").is_empty());
    }

    /// The syllables drawn: (middle x, baseline y, text).
    fn syllables(s: &Engraving) -> Vec<(f32, f32, &str)> {
        s.items
            .iter()
            .filter_map(|i| match i {
                Item::Text { x, y, text, size, anchor: Anchor::Middle, .. } if *size == LYRIC_SIZE => {
                    Some((*x, *y, text.as_str()))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn syllables_stand_under_their_notes() {
        let s = score("X:1\nT:T\nL:1/4\nK:C\nc d e f|\nw:do re mi fa\n");
        let sung = syllables(&s);
        let heads = noteheads(&s);
        assert_eq!(sung.iter().map(|t| t.2).collect::<Vec<_>>(), vec!["do", "re", "mi", "fa"]);
        let bottom_line = staff_tops(&s)[0] + 4.0;
        for (syllable, head) in sung.iter().zip(&heads) {
            // centred on the notehead, on one line below the staff
            assert!((syllable.0 - (head.0 + HEAD_W / 2.0)).abs() < 1e-4);
            assert_eq!(syllable.1, sung[0].1);
            assert!(syllable.1 - LYRIC_SIZE * 0.75 > bottom_line);
        }
        assert!(s.diagnostics.is_empty());
    }

    #[test]
    fn lyrics_stay_clear_of_low_notes() {
        let high = score("X:1\nT:T\nL:1/4\nK:C\nc d e f|\nw:do re mi fa\n");
        let low = score("X:1\nT:T\nL:1/4\nK:C\nc d G, f|\nw:do re mi fa\n");
        let depth = |s: &Engraving| syllables(s)[0].1 - staff_tops(s)[0];
        assert!(depth(&low) > depth(&high) + 1.0);
    }

    #[test]
    fn verses_are_rows() {
        let s = score("X:1\nT:T\nL:1/4\nK:C\nc d e f|\nw:do re mi fa\nw:one two three four\n");
        let sung = syllables(&s);
        assert_eq!(sung.len(), 8);
        for k in 0..4 {
            assert_eq!(sung[k].1, sung[0].1);
            assert_eq!(sung[k + 4].0, sung[k].0);
            assert!((sung[k + 4].1 - sung[k].1 - LYRIC_SIZE * LYRIC_LEADING).abs() < 1e-4);
        }
    }

    #[test]
    fn long_syllables_push_the_notes_apart() {
        let plain = score("X:1\nT:T\nL:1/4\nK:C\nc d e f|\n");
        let sung = score("X:1\nT:T\nL:1/4\nK:C\nc d e f|\nw:strengths strengths strengths strengths\n");
        let step = |s: &Engraving| noteheads(s)[1].0 - noteheads(s)[0].0;
        let width = crate::text_width("strengths", LYRIC_SIZE);
        assert!(step(&plain) < width);
        assert!(step(&sung) >= width + LYRIC_PAD - 1e-4);
        // also with a rest in between, which has no syllable of its own
        let s = score("X:1\nT:T\nL:1/4\nK:C\nc/ z/ d/|\nw:strengths strengths\n");
        let heads = noteheads(&s);
        assert!(heads[1].0 - heads[0].0 >= width + LYRIC_PAD - 1e-4);
    }

    #[test]
    fn hyphens_and_held_syllables() {
        let s = score("X:1\nT:T\nL:1/4\nK:C\nc d e f|\nw:hel-lo time_\n");
        let sung = syllables(&s);
        assert_eq!(sung.iter().map(|t| t.2).collect::<Vec<_>>(), vec!["hel", "-", "lo", "time"]);
        // the hyphen stands between its syllables
        assert!(sung[0].0 < sung[1].0 && sung[1].0 < sung[2].0);
        // a line runs from the held syllable to under the last note
        let lines: Vec<(f32, f32)> = s
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Line { x1, y1, x2, y2, width } if *width == LYRIC_LINE && y1 == y2 => Some((*x1, *x2)),
                _ => None,
            })
            .collect();
        assert_eq!(lines.len(), 1);
        let last_head = noteheads(&s)[3].0;
        assert!(lines[0].0 > sung[3].0 && (lines[0].1 - (last_head + HEAD_W)).abs() < 1e-4);
        // a word that goes on in the next system ends with its hyphen
        let s = score("X:1\nT:T\nL:1/4\nK:C\nc d|\ne f|\nw:a hel-lo there\n");
        let hyphens = s
            .items
            .iter()
            .filter(|i| matches!(i, Item::Text { text, anchor: Anchor::Start, .. } if text == "-"))
            .count();
        assert_eq!(hyphens, 1);
    }

    #[test]
    fn lyrics_lead_back_to_their_source() {
        let src = "X:1\nT:T\nL:1/4\nK:C\nc d e f|\nw:do re_ fa\n";
        let s = score(src);
        let bottom_line = staff_tops(&s)[0] + 4.0;
        for text in ["do", "re", "_", "fa"] {
            let at = src.rfind(text).unwrap();
            let hit = s.hits_at_source(at).next().unwrap_or_else(|| panic!("no hit for {}", text));
            assert!(hit.rect.y0 > bottom_line);
            assert_eq!(&src[hit.span.start..hit.span.end], text);
        }
    }

    #[test]
    fn each_voice_sings_under_its_own_staff() {
        let s = score("X:1\nT:T\nL:1/4\nK:C\n[V:1]c d|\nw:one two\n[V:2]C D|\nw:three four\n");
        let tops = staff_tops(&s);
        let sung = syllables(&s);
        assert_eq!(sung.iter().map(|t| t.2).collect::<Vec<_>>(), vec!["one", "two", "three", "four"]);
        // the first voice's words lie between the staves, and make room for themselves
        assert!(sung[0].1 > tops[0] + 4.0 && sung[0].1 < tops[1]);
        assert!(sung[2].1 > tops[1] + 4.0);
        assert!(tops[1] > sung[0].1 + 1.0);
        // notes that sound together still stand above each other
        let heads = noteheads(&s);
        assert_eq!(heads[0].0, heads[2].0);
        assert_eq!(heads[1].0, heads[3].0);
    }

    #[test]
    fn lyrics_go_on_over_systems() {
        let s = score("X:1\nT:T\nL:1/4\nK:C\nc d|\ne f|\nw:one two three four\n");
        let tops = staff_tops(&s);
        let sung = syllables(&s);
        assert_eq!(sung.len(), 4);
        assert!(sung[1].1 < tops[1] && sung[2].1 > tops[1]);
    }

    #[test]
    fn lyrics_without_notes_are_reported() {
        let src = "X:1\nT:T\nL:1/4\nK:C\nc d|\nw:one two three four\n";
        let s = score(src);
        assert_eq!(syllables(&s).len(), 2);
        assert_eq!(s.diagnostics.len(), 1);
        let d = &s.diagnostics[0];
        assert_eq!(&src[d.span.start..d.span.end], "three four");
    }

    #[test]
    fn words_are_printed_after_the_tune() {
        let src = "X:1\nT:T\nK:C\nABcd|\nW:The first line\nW:\nW:and the third\n";
        let s = score(src);
        let below = staff_tops(&s)[0] + 4.0;
        let words: Vec<(f32, f32, &str)> = s
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Text { x, y, text, anchor: Anchor::Start, .. } => Some((*x, *y, text.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(words.iter().map(|w| w.2).collect::<Vec<_>>(), vec!["The first line", "and the third"]);
        assert!(words[0].1 > below);
        // one block: the same left edge, and the empty line leaves a gap
        assert_eq!(words[0].0, words[1].0);
        assert!((words[1].1 - words[0].1 - 2.0 * WORDS_SIZE * 1.3).abs() < 1e-4);
        assert!(s.height > words[1].1);
        assert!(s.hits_at_source(src.find("and the third").unwrap()).next().is_some());
    }

    #[test]
    fn text_widths() {
        // Times: "strengths" is far narrower than nine half-size characters
        let w = crate::text_width("strengths", 1.0);
        assert!((w - 3.611).abs() < 1e-3);
        assert!(crate::text_width("WWW", 1.0) > crate::text_width("iii", 1.0) * 3.0);
        assert_eq!(crate::text_width("", 2.0), 0.0);
        // other alphabets get an estimate
        assert!(crate::text_width("ÉÀ", 1.0) > crate::text_width("éà", 1.0));
    }

    /// A tune of two bars for a soprano and a bass, with `header` before its `K:`.
    fn duet(header: &str) -> Engraving {
        score(&format!(
            "X:1\nT:T\nL:1/4\nV:S name=Sop\nV:B clef=bass name=Bass\n{}K:C\n[V:S]c d|e f|\n[V:B]C, D,|E, F,|\n",
            header
        ))
    }

    fn glyph_ys(s: &Engraving, c: char) -> Vec<f32> {
        s.items
            .iter()
            .filter_map(|i| match i {
                Item::Glyph { y, glyph, .. } if *glyph == c => Some(*y),
                _ => None,
            })
            .collect()
    }

    /// Where the staff lines start.
    fn staff_start(s: &Engraving) -> f32 {
        s.items
            .iter()
            .find_map(|i| match i {
                Item::Line { x1, y1, y2, width, .. } if *width == g::STAFF_LINE && y1 == y2 => Some(*x1),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn score_orders_the_staves() {
        let plain = duet("");
        assert!(glyph_ys(&plain, g::G_CLEF)[0] < glyph_ys(&plain, g::F_CLEF)[0]);
        let turned = duet("%%score B S\n");
        assert!(glyph_ys(&turned, g::F_CLEF)[0] < glyph_ys(&turned, g::G_CLEF)[0]);
        assert_eq!(texts(&turned), vec!["T", "Bass", "Sop"]);
        assert!(turned.diagnostics.is_empty());
        // voices in parentheses share a staff
        assert_eq!(staff_tops(&duet("%%score (S B)\n")).len(), 1);
    }

    #[test]
    fn voices_that_are_not_named_are_not_printed() {
        let s = duet("%%score B\n");
        assert_eq!(staff_tops(&s).len(), 1);
        assert!(glyph_ys(&s, g::G_CLEF).is_empty());
        assert_eq!(noteheads(&s).len(), 4);
        assert!(joins(&s).is_empty());
        // a voice the tune does not have is reported
        let s = duet("%%score S X\n");
        assert_eq!(staff_tops(&s).len(), 1);
        assert_eq!(s.diagnostics.len(), 1);
        assert_eq!(s.diagnostics[0].message, "the tune has no voice X");
    }

    /// The bar lines drawn in the gap between two staves.
    fn between(s: &Engraving) -> usize {
        let tops = staff_tops(s);
        s.items
            .iter()
            .filter(|i| matches!(i, Item::Line { x1, y1, x2, y2, .. } if x1 == x2 && *y1 == tops[0] + 4.0 && *y2 == tops[1]))
            .count()
    }

    #[test]
    fn bar_lines_run_through_joined_staves() {
        assert_eq!(between(&duet("")), 0);
        assert_eq!(between(&duet("%%score S B\n")), 0);
        assert_eq!(between(&duet("%%score [S B]\n")), 0);
        // both bar lines of the system
        assert_eq!(between(&duet("%%score S | B\n")), 2);
        assert_eq!(between(&duet("I:score S | B\n")), 2);
        // the older directive joins what is in a bracket or brace
        assert_eq!(between(&duet("%%staves [S B]\n")), 2);
        assert_eq!(between(&duet("%%staves {S B}\n")), 2);
        assert_eq!(between(&duet("%%staves [S | B]\n")), 0);
        assert_eq!(between(&duet("%%staves S B\n")), 0);
        // only where both staves have a bar line
        let s = score("X:1\nT:T\nL:1/4\n%%score S | B\nK:C\n[V:S]c d|e f|\n[V:B]C4|\n");
        assert_eq!(between(&s), 1);
    }

    #[test]
    fn a_bracket_stands_before_its_staves() {
        let plain = duet("");
        let s = duet("%%score [S B]\n");
        let tops = staff_tops(&s);
        assert_eq!(glyph_ys(&s, g::BRACKET_TOP), vec![tops[0]]);
        assert_eq!(glyph_ys(&s, g::BRACKET_BOTTOM), vec![tops[1] + 4.0]);
        let thick: Vec<(f32, f32, f32)> = s
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Line { x1, y1, y2, width, .. } if *width == BRACKET_LINE => Some((*x1, *y1, *y2)),
                _ => None,
            })
            .collect();
        assert_eq!(thick.len(), 1);
        assert_eq!((thick[0].1, thick[0].2), (tops[0], tops[1] + 4.0));
        // the staves make room for it, and the names stand to its left
        assert!(staff_start(&s) > staff_start(&plain) + BRACKET_LINE);
        assert!(thick[0].0 < staff_start(&s));
        for i in &s.items {
            if let Item::Text { x, anchor: Anchor::End, .. } = i {
                assert!(*x < thick[0].0 - BRACKET_LINE / 2.0);
            }
        }
        // a bracket around one staff of several
        let s = duet("%%score [S] B\n");
        let tops = staff_tops(&s);
        assert_eq!(glyph_ys(&s, g::BRACKET_BOTTOM), vec![tops[0] + 4.0]);
    }

    #[test]
    fn a_brace_stands_before_its_staves() {
        let outlines = |s: &Engraving| -> Vec<Vec<(f32, f32)>> {
            s.items
                .iter()
                .filter_map(|i| match i {
                    Item::Polygon { points } => Some(points.clone()),
                    _ => None,
                })
                .collect()
        };
        assert!(outlines(&duet("")).is_empty());
        let s = duet("%%score {S B}\n");
        let tops = staff_tops(&s);
        let halves = outlines(&s);
        assert_eq!(halves.len(), 2);
        let (top, bottom) = (tops[0], tops[1] + 4.0);
        let start = staff_start(&s);
        for p in halves.iter().flatten() {
            // left of the staves, and no taller than them
            assert!(p.0 < start && p.0 > start - BRACE_GAP - BRACE_WIDTH - 0.3);
            assert!(p.1 > top - 0.3 && p.1 < bottom + 0.3);
        }
        // the halves meet in the middle and reach the outer staff lines
        let ys: Vec<f32> = halves.iter().flatten().map(|p| p.1).collect();
        let (lo, hi) = (ys.iter().copied().fold(f32::MAX, f32::min), ys.iter().copied().fold(f32::MIN, f32::max));
        assert!((lo - top).abs() < 0.3 && (hi - bottom).abs() < 0.3);
        // a brace stands to the left of a bracket
        let s = duet("%%score [{S B}]\n");
        let bracket = s
            .items
            .iter()
            .find_map(|i| match i {
                Item::Line { x1, width, .. } if *width == BRACKET_LINE => Some(*x1),
                _ => None,
            })
            .unwrap();
        assert!(outlines(&s).iter().flatten().all(|p| p.0 < bracket - BRACKET_LINE / 2.0));
        // a tall brace is no fatter than a short one, only a little wider
        assert!(brace_width(40.0) <= BRACE_WIDTH && brace_width(40.0) > brace_width(13.0));
    }

    /// Two voices on one staff: `upper` and `lower` are their music, in L:1/4.
    fn pair(upper: &str, lower: &str) -> Engraving {
        score(&format!("X:1\nT:T\nL:1/4\n%%score (1 2)\nK:C\n[V:1]{}\n[V:2]{}\n", upper, lower))
    }

    /// For every stem, in drawing order: (x, goes up).
    fn stems(s: &Engraving) -> Vec<(f32, bool)> {
        s.items
            .iter()
            .filter_map(|i| match i {
                Item::Line { x1, y1, y2, width, .. } if (*width - g::STEM).abs() < 1e-6 => Some((*x1, y2 < y1)),
                _ => None,
            })
            .collect()
    }

    fn glyphs(s: &Engraving, c: char) -> Vec<(f32, f32)> {
        s.items
            .iter()
            .filter_map(|i| match i {
                Item::Glyph { x, y, glyph, .. } if *glyph == c => Some((*x, *y)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn two_voices_share_a_staff() {
        let s = pair("e f g a|", "c d e f|");
        // one staff, drawn once, with one clef and each bar line once
        assert_eq!(staff_tops(&s).len(), 1);
        assert_eq!(glyphs(&s, g::G_CLEF).len(), 1);
        assert!(joins(&s).is_empty());
        let bars = s
            .items
            .iter()
            .filter(|i| matches!(i, Item::Line { x1, x2, width, .. } if x1 == x2 && *width == g::THIN_BARLINE))
            .count();
        assert_eq!(bars, 1);
        // the upper voice's stems go up, the lower voice's down
        let stems = stems(&s);
        assert_eq!(stems.iter().map(|s| s.1).collect::<Vec<_>>(), vec![true, true, true, true, false, false, false, false]);
        // a third apart: the heads stand above each other
        let heads = noteheads(&s);
        assert_eq!(heads.len(), 8);
        for k in 0..4 {
            assert_eq!(heads[k].0, heads[k + 4].0);
            assert!(heads[k].1 < heads[k + 4].1);
        }
        assert!(s.diagnostics.is_empty());
        // repeat endings are drawn once, whichever voice has them
        for (upper, lower) in [("[1 e f|", "[1 c d|"), ("e f|", "[1 c d|")] {
            let s = pair(upper, lower);
            assert_eq!(texts(&s).iter().filter(|t| **t == "1.").count(), 1);
        }
        // both bar lines lead back to their own source
        let src = "X:1\nT:T\nL:1/4\n%%score (1 2)\nK:C\n[V:1]e f g a|\n[V:2]c d e f|\n";
        let s = score(src);
        assert!(s.hits_at_source(src.find("a|").unwrap() + 1).next().is_some());
        assert!(s.hits_at_source(src.rfind('|').unwrap()).next().is_some());
    }

    #[test]
    fn voices_on_one_staff_take_its_first_clef() {
        let s = score("X:1\nT:T\nL:1/4\n%%score (1 2)\nV:1 clef=bass\nV:2 clef=treble\nK:C\n[V:1]D,|\n[V:2]D,|\n");
        assert_eq!(glyphs(&s, g::F_CLEF).len(), 1);
        assert!(glyphs(&s, g::G_CLEF).is_empty());
        // the same note in both: on the middle line of the bass staff
        let top = staff_tops(&s)[0];
        assert!(noteheads(&s).iter().all(|h| h.1 - top == 2.0));
    }

    #[test]
    fn seconds_stand_side_by_side() {
        // the lower voice a second below: its head moves right until the stems are in line
        let s = pair("c|", "B|");
        let heads = noteheads(&s);
        assert!((heads[1].0 - heads[0].0 - (HEAD_W - g::STEM)).abs() < 1e-4);
        let lines = stems(&s);
        assert!((lines[0].0 - lines[1].0).abs() < 1e-4);
        // further apart: nothing moves
        let s = pair("c|", "A|");
        assert_eq!(noteheads(&s)[0].0, noteheads(&s)[1].0);
        // the voices crossed by a second: the upper voice stands to the right,
        // clear of the lower one, and the stems are on the outside
        let s = pair("B|", "c|");
        let heads = noteheads(&s);
        assert!(heads[0].0 - heads[1].0 > HEAD_W);
        let lines = stems(&s);
        assert!(lines[0].0 - lines[1].0 > 2.0 * HEAD_W - 2.0 * g::STEM);
        // stems that go the same way: the second voice stands clear of the first
        let s = score("X:1\nT:T\nL:1/4\n%%score (1 2)\nV:2 stem=up\nK:C\n[V:1]c|\n[V:2]B|\n");
        let heads = noteheads(&s);
        assert!(heads[1].0 - heads[0].0 > HEAD_W);
    }

    #[test]
    fn unisons_share_a_head_when_they_can() {
        // two quarter notes: one head, a stem each way
        let s = pair("c|", "c|");
        let heads = noteheads(&s);
        assert_eq!(heads[0], heads[1]);
        assert_eq!(stems(&s).iter().map(|s| s.1).collect::<Vec<_>>(), vec![true, false]);
        // a half note and a quarter note cannot: side by side, the stems in one line
        let s = pair("c2|", "c B|");
        let half = glyphs(&s, g::NOTEHEAD_HALF)[0];
        let quarter = noteheads(&s)[0];
        assert!(quarter.0 - half.0 > 1.0);
        let lines = stems(&s);
        assert!((lines[0].0 - lines[1].0).abs() < 1e-4);
        // nor a dotted note and a plain one; the dot stands to the right of both
        let s = pair("c3/2 B/|", "c B|");
        let heads = noteheads(&s);
        assert!(heads[2].0 - heads[0].0 > 1.0);
        let dot = glyphs(&s, g::AUGMENTATION_DOT)[0];
        assert!(dot.0 > heads[2].0 + HEAD_W);
    }

    #[test]
    fn dots_of_two_voices() {
        // notes on lines: the upper voice's dot goes into the space above,
        // the lower voice's into the space below
        let s = pair("d3/2 d/|", "B3/2 B/|");
        let top = staff_tops(&s)[0];
        let dots: Vec<f32> = glyphs(&s, g::AUGMENTATION_DOT).iter().map(|d| d.1 - top).collect();
        assert_eq!(dots, vec![0.5, 2.5]);
        // on a staff of its own the dot goes into the space above
        let s = score("X:1\nT:T\nL:1/4\nK:C\nB3/2 B/|\n");
        assert_eq!(glyphs(&s, g::AUGMENTATION_DOT)[0].1 - staff_tops(&s)[0], 1.5);
    }

    #[test]
    fn notes_between_the_beats_of_another_voice() {
        // the eighth note after the dotted quarter stands between the
        // quarter notes of the other voice, clear of both
        let s = pair("c3/2 B/|", "A A|");
        let heads = noteheads(&s);
        // drawn: c B (first voice), A A (second voice)
        let (eighth, second) = (heads[1].0, heads[3].0);
        assert!(eighth - second > HEAD_W + 0.4);
        // the same on two staves
        let s = score("X:1\nT:T\nL:1/4\nK:C\n[V:1]c3/2 B/ c|\n[V:2]A A A|\n");
        let heads = noteheads(&s);
        let (eighth, second, third) = (heads[1].0, heads[4].0, heads[5].0);
        assert!(eighth - second > 1.5 && third - eighth > 1.5);
    }

    #[test]
    fn the_notes_after_a_shifted_note_keep_their_distance() {
        let plain = pair("c d|", "A B|");
        let shifted = pair("c d|", "B B|");
        let step = |s: &Engraving| noteheads(s)[1].0 - noteheads(s)[0].0;
        assert!(step(&shifted) >= step(&plain));
        // the second voice's next note is clear of its shifted one
        let heads = noteheads(&shifted);
        assert!(heads[3].0 - heads[2].0 > HEAD_W + 0.5);
    }

    #[test]
    fn accidentals_of_two_voices_do_not_cover_each_other() {
        let s = pair("^c|", "^A|");
        let sharps = glyphs(&s, g::SHARP);
        assert_eq!(sharps.len(), 2);
        assert!((sharps[0].0 - sharps[1].0).abs() >= g::width(g::SHARP));
        // far apart they stand above each other
        let s = pair("^c'|", "^A,|");
        let sharps = glyphs(&s, g::SHARP);
        assert_eq!(sharps[0].0, sharps[1].0);
        // the accidental of a shifted note stands before both voices
        let s = pair("c|", "^B|");
        let heads = noteheads(&s);
        let sharp = glyphs(&s, g::SHARP)[0];
        assert!(sharp.0 + g::width(g::SHARP) < heads[0].0);
    }

    #[test]
    fn rests_of_two_voices() {
        let rest_ys = |s: &Engraving| -> Vec<f32> {
            let top = staff_tops(s)[0];
            glyphs(s, g::REST_QUARTER).iter().map(|r| r.1 - top).collect()
        };
        // the same rest in both voices is drawn once, in the middle
        assert_eq!(rest_ys(&pair("c z|", "A z|")), vec![2.0]);
        // a rest against a note moves to its voice's side
        assert_eq!(rest_ys(&pair("z c|", "A A|")), vec![0.0]);
        assert_eq!(rest_ys(&pair("c c|", "z A|")), vec![4.0]);
        // different rests at the same time both move
        let s = pair("z c|", "z2|");
        let top = staff_tops(&s)[0];
        assert_eq!(rest_ys(&s), vec![0.0]);
        assert_eq!(glyphs(&s, g::REST_HALF)[0].1 - top, 4.0);
        // whole rests stay on a line of the staff
        let s = pair("z4|", "A A A A|");
        assert_eq!(glyphs(&s, g::REST_WHOLE)[0].1 - staff_tops(&s)[0], 0.0);
        // a rest goes on, by whole spaces, until it is clear of the other voice's note
        assert_eq!(rest_ys(&pair("z c|", "e A|")), vec![-2.0]);
        assert_eq!(rest_ys(&pair("D c|", "z A|")), vec![7.0]);
        // outside the staff a whole rest hangs from a ledger line
        let ledgers = |s: &Engraving| {
            s.items
                .iter()
                .filter(|i| matches!(i, Item::Line { y1, y2, width, .. } if y1 == y2 && *width == g::LEDGER_LINE))
                .count()
        };
        let s = pair("a4|", "z4|");
        assert_eq!(ledgers(&s), 1);
        let s = pair("E4|", "z4|");
        assert_eq!(glyphs(&s, g::REST_WHOLE)[0].1 - staff_tops(&s)[0], 5.0);
        assert_eq!(ledgers(&s), 1);
        // a voice that is silent and unseen leaves the other alone
        assert_eq!(rest_ys(&pair("z c|", "x x|")), vec![2.0]);
        assert_eq!(rest_ys(&pair("z c|", "")), vec![2.0]);
        // and on a staff of its own a rest stays where it was
        let s = score("X:1\nT:T\nL:1/4\nK:C\n[V:1]z c|\n[V:2]A A|\n");
        assert_eq!(glyphs(&s, g::REST_QUARTER)[0].1 - staff_tops(&s)[0], 2.0);
    }

    #[test]
    fn marks_stay_on_their_voices_side() {
        let s = pair("Hc2 (c d)|", "HA2 (A B)|");
        let top = staff_tops(&s)[0];
        // the lower voice's fermata hangs under the staff
        assert_eq!(glyphs(&s, g::FERMATA_ABOVE).len(), 1);
        assert!(glyphs(&s, g::FERMATA_ABOVE)[0].1 < top);
        assert_eq!(glyphs(&s, g::FERMATA_BELOW).len(), 1);
        assert!(glyphs(&s, g::FERMATA_BELOW)[0].1 > top + 4.0);
        // one slur above the staff's notes, one below
        let slurs: Vec<f32> = s
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Curve { p0, c1, .. } => Some(c1.1 - p0.1),
                _ => None,
            })
            .collect();
        assert_eq!(slurs.len(), 2);
        assert!(slurs[0] < 0.0 && slurs[1] > 0.0);
        // a slur may end on a rest
        let s = score("X:1\nT:T\nL:1/4\nK:C\n(A z) (z A) (A x)|\n");
        assert!(s.height.is_finite() && s.height < 40.0);
        // ties curve away from the other voice too
        let s = pair("c-c|", "A-A|");
        let ties: Vec<f32> = s
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Curve { p0, c1, .. } => Some(c1.1 - p0.1),
                _ => None,
            })
            .collect();
        assert_eq!(ties.len(), 2);
        assert!(ties[0] < 0.0 && ties[1] > 0.0);
        // staccato dots go beyond the stems
        let s = pair(".c|", ".A|");
        let heads = noteheads(&s);
        assert!(glyphs(&s, g::STACCATO_ABOVE)[0].1 < heads[0].1 - 3.0);
        assert!(glyphs(&s, g::STACCATO_BELOW)[0].1 > heads[1].1 + 3.0);
    }

    #[test]
    fn names_of_voices_on_one_staff() {
        let s = score("X:1\nT:T\nL:1/4\n%%score (S A) B\nV:S name=Sop\nV:A name=Alto\nV:B name=Bass\nK:C\n[V:S]c|\n[V:A]A|\n[V:B]C|\n");
        let tops = staff_tops(&s);
        assert_eq!(tops.len(), 2);
        let names: Vec<(&str, f32)> = s
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Text { text, y, anchor: Anchor::End, .. } => Some((text.as_str(), *y)),
                _ => None,
            })
            .collect();
        assert_eq!(names.iter().map(|n| n.0).collect::<Vec<_>>(), vec!["Sop", "Alto", "Bass"]);
        // the two names of the first staff stand above each other, around its middle
        let middle = tops[0] + 2.0 + LABEL_SIZE * 0.3;
        assert!(names[0].1 < middle && names[1].1 > middle);
        assert!((names[0].1 + names[1].1 - 2.0 * middle).abs() < 1e-4);
        assert!((names[2].1 - (tops[1] + 2.0 + LABEL_SIZE * 0.3)).abs() < 1e-4);
        // one line joins the two staves, not the three voices
        assert_eq!(joins(&s).len(), 2);
    }

    #[test]
    fn lyrics_of_voices_on_one_staff() {
        let s = score("X:1\nT:T\nL:1/4\n%%score (1 2)\nK:C\n[V:1]e f|\nw:one two\n[V:2]C, D,|\nw:three four\n");
        let sung = syllables(&s);
        assert_eq!(sung.iter().map(|t| t.2).collect::<Vec<_>>(), vec!["one", "two", "three", "four"]);
        // both below the low notes of the second voice, the first voice's words first
        let lowest = noteheads(&s).iter().map(|h| h.1).fold(f32::MIN, f32::max);
        assert!(sung[0].1 - LYRIC_SIZE * 0.75 > lowest);
        assert!((sung[2].1 - sung[0].1 - LYRIC_SIZE * LYRIC_LEADING).abs() < 1e-4);
    }

    #[test]
    fn three_voices_on_one_staff() {
        let s = score("X:1\nT:T\nL:1/4\n%%score (1 2 3)\nK:C\n[V:1]g|\n[V:2]c|\n[V:3]e|\n");
        assert_eq!(staff_tops(&s).len(), 1);
        assert_eq!(stems(&s).iter().map(|s| s.1).collect::<Vec<_>>(), vec![true, false, true]);
        // stem=down turns a voice over
        let s = score("X:1\nT:T\nL:1/4\n%%score (1 2)\nV:1 stem=down\nV:2 stem=up\nK:C\n[V:1]c|\n[V:2]g|\n");
        assert_eq!(stems(&s).iter().map(|s| s.1).collect::<Vec<_>>(), vec![false, true]);
    }

    #[test]
    fn an_ampersand_makes_a_second_voice_for_its_bar() {
        let src = "X:1\nT:T\nL:1/4\nK:C\ne f & c d|g a|\n";
        let s = score(src);
        assert!(s.diagnostics.is_empty());
        assert_eq!(staff_tops(&s).len(), 1);
        let heads = noteheads(&s);
        assert_eq!(heads.len(), 6);
        // in drawing order: e f g a, then c d under e f
        assert_eq!(heads[4].0, heads[0].0);
        assert_eq!(heads[5].0, heads[1].0);
        assert!(heads[4].1 > heads[0].1);
        // in the bar with two voices the stems point away from each other; the next bar is as ever
        let stems = stems(&s);
        assert_eq!(stems.iter().map(|s| s.1).collect::<Vec<_>>(), vec![true, true, false, false, false, false]);
        // the staff still ends with its bar line
        let alone = score("X:1\nT:T\nL:1/4\nK:C\ne f|g a|\n");
        let end = |s: &Engraving| {
            s.items
                .iter()
                .find_map(|i| match i {
                    Item::Line { x2, y1, y2, width, .. } if *width == g::STAFF_LINE && y1 == y2 => Some(*x2),
                    _ => None,
                })
                .unwrap()
        };
        assert_eq!(end(&s), end(&alone));
        // every note leads back to its source
        for note in ["e", "f", "c", "d", "g", "a"] {
            assert!(s.hits_at_source(src.rfind(note).unwrap()).next().is_some());
        }
    }

    #[test]
    fn an_ampersand_in_a_later_bar_and_twice_in_a_bar() {
        let s = score("X:1\nT:T\nL:1/4\nK:C\ne f|g a & c d|\n");
        assert!(s.diagnostics.is_empty());
        let heads = noteheads(&s);
        assert_eq!(heads[4].0, heads[2].0);
        assert_eq!(heads[5].0, heads[3].0);
        // the bar line stands after the music that follows the &, too
        let s = score("X:1\nT:T\nL:1/4\nK:C\ne4 & c d e f|\n");
        assert!(s.diagnostics.is_empty());
        let last = noteheads(&s).iter().map(|h| h.0).fold(f32::MIN, f32::max);
        let bar = s
            .items
            .iter()
            .find_map(|i| match i {
                Item::Line { x1, x2, width, .. } if x1 == x2 && *width == g::THIN_BARLINE => Some(*x1),
                _ => None,
            })
            .unwrap();
        assert!(bar > last + HEAD_W + 0.5);
        // three voices in one bar
        let s = score("X:1\nT:T\nL:1/4\nK:C\ng2 & e2 & c2|\n");
        assert!(s.diagnostics.is_empty());
        let heads = glyphs(&s, g::NOTEHEAD_HALF);
        assert_eq!(heads.len(), 3);
        assert!(heads.iter().all(|h| h.0 == heads[0].0));
        assert_eq!(stems(&s).iter().map(|s| s.1).collect::<Vec<_>>(), vec![true, false, true]);
        // in a voice of several, with lyrics for the voice itself only
        let s = score("X:1\nT:T\nL:1/4\nK:C\n[V:1]e f & c d|\nw:one two\n[V:2]C2|\n");
        assert!(s.diagnostics.is_empty());
        assert_eq!(staff_tops(&s).len(), 2);
        let sung = syllables(&s);
        let heads = noteheads(&s);
        assert_eq!(sung.len(), 2);
        assert!((sung[0].0 - (heads[0].0 + HEAD_W / 2.0)).abs() < 1e-4);
    }

    #[test]
    fn music_after_an_ampersand_fills_its_bar() {
        let src = "X:1\nT:T\nL:1/4\nK:C\ne f & c|g a|\n";
        let s = score(src);
        assert_eq!(s.diagnostics.len(), 1);
        let d = &s.diagnostics[0];
        assert_eq!(&src[d.span.start..d.span.end], "& c");
        assert_eq!(d.message, "the music after this & is 1 units of L:1/4 long, its bar is 2");
    }

    #[test]
    fn a_score_of_shared_staves() {
        let src = "X:1\nT:T\nM:C\nL:1/8\n%%score (T1 T2) (B1 B2)\nV:T1 name=\"Tenore I\"\nV:T2 name=\"Tenore II\"\nV:B1 clef=bass name=\"Basso I\"\nV:B2 clef=bass name=\"Basso II\"\nK:Gm\n[V:T1] (B2c2 d2g2) | f8 | d3c (d2fe) | H d6 z2 ||\n[V:T2] z8 | z8 | B3A (B2c2) | H A6 z2 ||\n[V:B1] z8 | z2F2 G2A2 | G3G G4 | H^F6 z2 ||\n[V:B2] x8 | z2B,2 C2D2 | E3E (D2C2) | H D6 z2 ||\n";
        let s = score(src);
        assert!(s.diagnostics.is_empty());
        let tops = staff_tops(&s);
        assert_eq!(tops.len(), 2);
        assert_eq!((glyphs(&s, g::G_CLEF).len(), glyphs(&s, g::F_CLEF).len()), (1, 1));
        // the staves are joined on the left and closed on the right
        assert_eq!(joins(&s).len(), 3);
        // four fermatas: two above their staves, two below
        assert_eq!((glyphs(&s, g::FERMATA_ABOVE).len(), glyphs(&s, g::FERMATA_BELOW).len()), (2, 2));
        // the rests at the end, the same in both voices of a staff, are drawn once for each staff
        assert_eq!(glyphs(&s, g::REST_QUARTER).len(), 2 + 1);
    }
}
