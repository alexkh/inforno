//! The meaning of `K:`, `M:` and `L:` fields, and of note durations.

use crate::glyphs;
use num_rational::Rational32;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Clef {
    Treble,
    Bass,
    Alto,
    Tenor,
}

impl Clef {
    /// Diatonic number (C4 = 0) of the top staff line.
    pub fn top_line(self) -> i32 {
        match self {
            Clef::Treble => 10, // F5
            Clef::Bass => -2,   // A3
            Clef::Alto => 4,    // G4
            Clef::Tenor => 2,   // E4
        }
    }

    pub fn glyph(self) -> char {
        match self {
            Clef::Treble => glyphs::G_CLEF,
            Clef::Bass => glyphs::F_CLEF,
            Clef::Alto | Clef::Tenor => glyphs::C_CLEF,
        }
    }

    /// Staff position (staff spaces below the top line) of the glyph's baseline.
    pub fn glyph_y(self) -> f32 {
        match self {
            Clef::Treble => 3.0,
            Clef::Bass => 1.0,
            Clef::Alto => 2.0,
            Clef::Tenor => 1.0,
        }
    }

    /// How far the key signature sits below the treble positions, in diatonic steps.
    fn key_shift(self) -> i32 {
        match self {
            Clef::Treble => 0,
            Clef::Bass => 14,
            Clef::Alto => 7,
            Clef::Tenor => 9,
        }
    }

    /// Diatonic positions of the accidentals of a key signature.
    pub fn key_positions(self, fifths: i32) -> Vec<i32> {
        const SHARPS: [i32; 7] = [10, 7, 11, 8, 5, 9, 6]; // F5 C5 G5 D5 A4 E5 B4
        const FLATS: [i32; 7] = [6, 9, 5, 8, 4, 7, 3]; // B4 E5 A4 D5 G4 C5 F4
        let shift = self.key_shift();
        let mut sharps = SHARPS;
        if self == Clef::Tenor {
            // the first sharp would sit below the staff
            sharps[0] -= 7;
            sharps[2] -= 7;
        }
        let list: &[i32] = if fifths >= 0 { &sharps } else { &FLATS };
        list.iter()
            .take(fifths.unsigned_abs().min(7) as usize)
            .map(|d| d - shift)
            .collect()
    }
}

/// A clef by its lower-case name; an octave mark such as `-8` is skipped.
pub(crate) fn clef_name(name: &str) -> Option<Clef> {
    let name = name.trim_end_matches(['8', '+', '-']);
    match name {
        "treble" | "g" => Some(Clef::Treble),
        "bass" | "f" => Some(Clef::Bass),
        "alto" | "c" => Some(Clef::Alto),
        "tenor" => Some(Clef::Tenor),
        _ => None,
    }
}

fn mode_offset(mode: &str) -> Option<i32> {
    let m = mode.to_ascii_lowercase();
    if m == "m" || m.starts_with("min") {
        return Some(-3);
    }
    match m.get(..3)? {
        "maj" | "ion" => Some(0),
        "mix" => Some(-1),
        "dor" => Some(-2),
        "aeo" => Some(-3),
        "phr" => Some(-4),
        "loc" => Some(-5),
        "lyd" => Some(1),
        _ => None,
    }
}

/// A `K:` field: the key signature and an optional clef change.
#[derive(Clone, Debug, PartialEq)]
pub struct Key {
    /// Sharps (positive) or flats (negative).
    pub fifths: i32,
    pub clef: Option<Clef>,
}

pub fn parse_key(value: &str) -> Key {
    let mut key = Key {
        fifths: 0,
        clef: None,
    };
    let mut first = true;
    let mut mode_pending = false;
    for tok in value.split_whitespace() {
        let lower = tok.to_ascii_lowercase();
        if let Some(name) = lower.strip_prefix("clef=") {
            key.clef = clef_name(name).or(key.clef);
            continue;
        }
        if lower.contains('=') && !tok.starts_with('=') {
            continue; // middle=, transpose=, octave=...
        }
        if let Some(c) = clef_name(&lower).filter(|_| lower.len() > 1) {
            key.clef = Some(c);
            continue;
        }
        if first {
            first = false;
            if lower == "none" || tok == "HP" {
                continue;
            }
            if tok == "Hp" {
                key.fifths = 2;
                continue;
            }
            let mut chars = tok.chars();
            let base = match chars.next().map(|c| c.to_ascii_uppercase()) {
                Some('C') => 0,
                Some('G') => 1,
                Some('D') => 2,
                Some('A') => 3,
                Some('E') => 4,
                Some('B') => 5,
                Some('F') => -1,
                _ => continue,
            };
            let mut rest = chars.as_str();
            let mut fifths = base;
            if let Some(r) = rest.strip_prefix('#') {
                fifths += 7;
                rest = r;
            } else if let Some(r) = rest.strip_prefix('b') {
                fifths -= 7;
                rest = r;
            }
            if rest.is_empty() {
                mode_pending = true;
            } else {
                fifths += mode_offset(rest).unwrap_or(0);
            }
            key.fifths = fifths.clamp(-7, 7);
            continue;
        }
        if mode_pending
            && let Some(off) = mode_offset(&lower) {
                key.fifths = (key.fifths + off).clamp(-7, 7);
                mode_pending = false;
            }
        // explicit accidentals such as "^f _b" are not drawn yet
    }
    key
}

/// An `M:` field.
#[derive(Clone, Debug, PartialEq)]
pub enum Meter {
    None,
    Common,
    Cut,
    Fraction { num: Vec<u32>, den: u32 },
}

impl Meter {
    /// The length of a bar, as a fraction of a whole note.
    pub fn value(&self) -> Option<Rational32> {
        match self {
            Meter::None => None,
            Meter::Common => Some(Rational32::from_integer(1)),
            Meter::Cut => Some(Rational32::from_integer(1)),
            Meter::Fraction { num, den } => {
                let n: u32 = num.iter().sum();
                Some(Rational32::new(n as i32, (*den).max(1) as i32))
            }
        }
    }

    pub fn is_compound(&self) -> bool {
        match self {
            Meter::Fraction { num, den } => {
                let n: u32 = num.iter().sum();
                num.len() == 1 && n > 3 && n.is_multiple_of(3) && *den >= 4
            }
            _ => false,
        }
    }
}

pub fn parse_meter(value: &str) -> Meter {
    let v = value.trim();
    match v {
        "" | "none" => return Meter::None,
        "C" => return Meter::Common,
        "C|" => return Meter::Cut,
        _ => {}
    }
    let Some((n, d)) = v.split_once('/') else {
        return Meter::None;
    };
    let n = n.trim().trim_start_matches('(').trim_end_matches(')');
    let num: Option<Vec<u32>> = n.split('+').map(|s| s.trim().parse().ok()).collect();
    match (num, d.trim().parse::<u32>()) {
        (Some(num), Ok(den)) if !num.is_empty() && den > 0 => Meter::Fraction { num, den },
        _ => Meter::None,
    }
}

/// An `L:` field such as `1/8`.
pub fn parse_length(value: &str) -> Option<Rational32> {
    let v = value.trim();
    match v.split_once('/') {
        Some((n, d)) => {
            let n: i32 = n.trim().parse().ok()?;
            let d: i32 = d.trim().parse().ok()?;
            (n > 0 && d > 0).then(|| Rational32::new(n, d))
        }
        None => v.parse::<i32>().ok().filter(|n| *n > 0).map(Rational32::from_integer),
    }
}

/// The unit length when a tune has no `L:` field.
pub fn default_unit(meter: &Meter) -> Rational32 {
    match meter.value() {
        Some(v) if v < Rational32::new(3, 4) => Rational32::new(1, 16),
        _ => Rational32::new(1, 8),
    }
}

/// How a duration is written: a basic value with dots.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct NoteValue {
    /// log2 of the basic value: 1 = breve, 0 = whole, -1 = half, -2 = quarter, -3 = eighth ...
    pub log: i32,
    pub dots: u8,
}

impl NoteValue {
    pub fn flags(&self) -> u8 {
        if self.log <= -3 {
            (-2 - self.log) as u8
        } else {
            0
        }
    }

    pub fn has_stem(&self) -> bool {
        self.log <= -1
    }

    pub fn notehead(&self) -> char {
        match self.log {
            l if l >= 1 => glyphs::NOTEHEAD_DOUBLE_WHOLE,
            0 => glyphs::NOTEHEAD_WHOLE,
            -1 => glyphs::NOTEHEAD_HALF,
            _ => glyphs::NOTEHEAD_BLACK,
        }
    }

    /// The sharp-notation notehead for this value.
    pub fn raised_shape(&self) -> crate::Shape {
        match self.log {
            l if l >= 0 => crate::Shape::RaisedWhole,
            -1 => crate::Shape::RaisedHalf,
            _ => crate::Shape::RaisedBlack,
        }
    }

    pub fn rest(&self) -> char {
        match self.log {
            l if l >= 0 => glyphs::REST_WHOLE,
            -1 => glyphs::REST_HALF,
            -2 => glyphs::REST_QUARTER,
            -3 => glyphs::REST_8TH,
            -4 => glyphs::REST_16TH,
            -5 => glyphs::REST_32ND,
            _ => glyphs::REST_64TH,
        }
    }
}

/// Splits a duration (fraction of a whole note) into a basic value and dots.
/// Durations that need a tie, such as 5/8, are shown as the largest value that fits.
pub fn note_value(d: Rational32) -> NoteValue {
    let mut log = 1;
    let mut base = Rational32::from_integer(2);
    while base > d && log > -8 {
        base /= 2;
        log -= 1;
    }
    let mut dots = 0;
    let mut total = base;
    let mut add = base / 2;
    while dots < 3 && total + add <= d {
        total += add;
        add /= 2;
        dots += 1;
    }
    if total != d {
        dots = 0;
    }
    NoteValue { log, dots }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        let f = |s: &str| parse_key(s).fifths;
        assert_eq!(f("C"), 0);
        assert_eq!(f("D"), 2);
        assert_eq!(f("Bb"), -2);
        assert_eq!(f("F#"), 6);
        assert_eq!(f("Am"), 0);
        assert_eq!(f("Em"), 1);
        assert_eq!(f("E minor"), 1);
        assert_eq!(f("Ador"), 1);
        assert_eq!(f("A dor"), 1);
        assert_eq!(f("Edor"), 2);
        assert_eq!(f("Dmix"), 1);
        assert_eq!(f("Bbm"), -5);
        assert_eq!(f("none"), 0);
        assert_eq!(f("Hp"), 2);
        assert_eq!(f(""), 0);
        assert_eq!(parse_key("G clef=bass").clef, Some(Clef::Bass));
        assert_eq!(parse_key("Am bass").clef, Some(Clef::Bass));
        assert_eq!(parse_key("D").clef, None);
    }

    #[test]
    fn key_positions() {
        assert_eq!(Clef::Treble.key_positions(2), vec![10, 7]);
        assert_eq!(Clef::Treble.key_positions(-1), vec![6]);
        assert_eq!(Clef::Bass.key_positions(1), vec![-4]);
    }

    #[test]
    fn meters() {
        assert_eq!(parse_meter("4/4"), Meter::Fraction { num: vec![4], den: 4 });
        assert_eq!(parse_meter("C"), Meter::Common);
        assert_eq!(parse_meter("C|"), Meter::Cut);
        assert_eq!(parse_meter("2+3/8"), Meter::Fraction { num: vec![2, 3], den: 8 });
        assert_eq!(parse_meter("none"), Meter::None);
        assert!(parse_meter("6/8").is_compound());
        assert!(!parse_meter("3/4").is_compound());
        assert_eq!(default_unit(&parse_meter("2/4")), Rational32::new(1, 16));
        assert_eq!(default_unit(&parse_meter("3/4")), Rational32::new(1, 8));
        assert_eq!(default_unit(&Meter::None), Rational32::new(1, 8));
    }

    #[test]
    fn lengths() {
        assert_eq!(parse_length("1/8"), Some(Rational32::new(1, 8)));
        assert_eq!(parse_length("1"), Some(Rational32::from_integer(1)));
        assert_eq!(parse_length("x"), None);
    }

    #[test]
    fn note_values() {
        let v = |n, d| note_value(Rational32::new(n, d));
        assert_eq!(v(1, 4), NoteValue { log: -2, dots: 0 });
        assert_eq!(v(3, 8), NoteValue { log: -2, dots: 1 });
        assert_eq!(v(7, 16), NoteValue { log: -2, dots: 2 });
        assert_eq!(v(1, 1), NoteValue { log: 0, dots: 0 });
        assert_eq!(v(3, 16), NoteValue { log: -3, dots: 1 });
        assert_eq!(v(5, 8), NoteValue { log: -1, dots: 0 });
        assert_eq!(v(1, 16).flags(), 2);
    }
}
