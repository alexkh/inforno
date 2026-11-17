//! SMuFL codepoints and Bravura metrics, in staff spaces.
//!
//! Codepoints come from the SMuFL glyph list; widths are the advance widths of
//! the Bravura font (1 em = 4 staff spaces).

pub const G_CLEF: char = '\u{E050}';
pub const C_CLEF: char = '\u{E05C}';
pub const F_CLEF: char = '\u{E062}';

pub const NOTEHEAD_DOUBLE_WHOLE: char = '\u{E0A0}';
pub const NOTEHEAD_WHOLE: char = '\u{E0A2}';
pub const NOTEHEAD_HALF: char = '\u{E0A3}';
pub const NOTEHEAD_BLACK: char = '\u{E0A4}';

pub const FLAT: char = '\u{E260}';
pub const NATURAL: char = '\u{E261}';
pub const SHARP: char = '\u{E262}';
pub const DOUBLE_SHARP: char = '\u{E263}';
pub const DOUBLE_FLAT: char = '\u{E264}';

pub const REST_WHOLE: char = '\u{E4E3}';
pub const REST_HALF: char = '\u{E4E4}';
pub const REST_QUARTER: char = '\u{E4E5}';
pub const REST_8TH: char = '\u{E4E6}';
pub const REST_16TH: char = '\u{E4E7}';
pub const REST_32ND: char = '\u{E4E8}';
pub const REST_64TH: char = '\u{E4E9}';

pub const FLAG_UP: [char; 4] = ['\u{E240}', '\u{E242}', '\u{E244}', '\u{E246}'];
pub const FLAG_DOWN: [char; 4] = ['\u{E241}', '\u{E243}', '\u{E245}', '\u{E247}'];

pub const TIME_SIG_DIGIT_0: u32 = 0xE080;
pub const TIME_SIG_COMMON: char = '\u{E08A}';
pub const TIME_SIG_CUT: char = '\u{E08B}';
pub const TIME_SIG_PLUS: char = '\u{E08C}';
pub const TUPLET_DIGIT_0: u32 = 0xE880;

pub const AUGMENTATION_DOT: char = '\u{E1E7}';
pub const REPEAT_DOT: char = '\u{E044}';

/// The curved ends of the bracket before a group of staves. Their origin is
/// the end of the bracket's thick line, at its left edge.
pub const BRACKET_TOP: char = '\u{E003}';
pub const BRACKET_BOTTOM: char = '\u{E004}';

pub const STACCATO_ABOVE: char = '\u{E4A2}';
pub const STACCATO_BELOW: char = '\u{E4A3}';
pub const ACCENT_ABOVE: char = '\u{E4A0}';
pub const ACCENT_BELOW: char = '\u{E4A1}';
pub const TENUTO_ABOVE: char = '\u{E4A4}';
pub const TENUTO_BELOW: char = '\u{E4A5}';
pub const MARCATO_ABOVE: char = '\u{E4AC}';
pub const FERMATA_ABOVE: char = '\u{E4C0}';
/// Hangs down from its origin.
pub const FERMATA_BELOW: char = '\u{E4C1}';
pub const BREATH_MARK: char = '\u{E4CE}';
pub const TRILL: char = '\u{E566}';
pub const TURN: char = '\u{E567}';
pub const SHORT_TRILL: char = '\u{E56C}';
pub const MORDENT: char = '\u{E56D}';
pub const SEGNO: char = '\u{E047}';
pub const CODA: char = '\u{E048}';
pub const DOWN_BOW: char = '\u{E610}';
pub const UP_BOW: char = '\u{E612}';

pub const DYN_P: char = '\u{E520}';
pub const DYN_M: char = '\u{E521}';
pub const DYN_F: char = '\u{E522}';
pub const DYN_R: char = '\u{E523}';
pub const DYN_S: char = '\u{E524}';
pub const DYN_Z: char = '\u{E525}';

/// Advance width of a glyph in staff spaces (0 for glyphs not listed).
pub fn width(c: char) -> f32 {
    match c {
        G_CLEF => 2.684,
        C_CLEF => 2.796,
        F_CLEF => 2.736,
        NOTEHEAD_DOUBLE_WHOLE => 2.396,
        NOTEHEAD_WHOLE => 1.688,
        NOTEHEAD_HALF | NOTEHEAD_BLACK => 1.18,
        FLAT => 0.904,
        NATURAL => 0.672,
        SHARP => 0.996,
        DOUBLE_SHARP => 0.988,
        DOUBLE_FLAT => 1.644,
        REST_WHOLE | REST_HALF => 1.128,
        REST_QUARTER => 1.08,
        REST_8TH => 0.988,
        REST_16TH => 1.28,
        REST_32ND => 1.452,
        REST_64TH => 1.692,
        TIME_SIG_COMMON => 1.696,
        TIME_SIG_CUT => 1.668,
        TIME_SIG_PLUS => 1.5,
        AUGMENTATION_DOT | REPEAT_DOT => 0.4,
        BRACKET_TOP | BRACKET_BOTTOM => 1.876,
        STACCATO_ABOVE | STACCATO_BELOW => 0.336,
        ACCENT_ABOVE | ACCENT_BELOW => 1.356,
        TENUTO_ABOVE | TENUTO_BELOW => 1.352,
        MARCATO_ABOVE => 0.944,
        FERMATA_ABOVE | FERMATA_BELOW => 2.42,
        BREATH_MARK => 0.612,
        TRILL => 2.084,
        TURN => 1.84,
        SHORT_TRILL => 2.92,
        MORDENT => 2.916,
        SEGNO => 2.228,
        CODA => 3.816,
        DOWN_BOW => 1.248,
        UP_BOW => 1.0,
        DYN_P => 1.46,
        DYN_M => 1.748,
        DYN_F => 1.456,
        DYN_R => 1.108,
        DYN_S => 0.916,
        DYN_Z => 0.976,
        c if (TIME_SIG_DIGIT_0..TIME_SIG_DIGIT_0 + 10).contains(&(c as u32)) => {
            [1.88, 1.336, 1.784, 1.684, 1.88, 1.612, 1.736, 1.764, 1.744, 1.736]
                [(c as u32 - TIME_SIG_DIGIT_0) as usize]
        }
        c if (TUPLET_DIGIT_0..TUPLET_DIGIT_0 + 10).contains(&(c as u32)) => {
            [1.276, 0.984, 1.276, 1.184, 1.212, 1.268, 1.216, 1.212, 1.252, 1.216]
                [(c as u32 - TUPLET_DIGIT_0) as usize]
        }
        _ => 0.0,
    }
}

/// Height above the baseline (staff spaces) of glyphs drawn above the staff,
/// used to stack decorations.
pub fn height(c: char) -> f32 {
    match c {
        FERMATA_ABOVE => 1.32,
        TRILL => 1.56,
        TURN => 0.87,
        SHORT_TRILL => 0.98,
        MORDENT => 1.28,
        SEGNO => 3.04,
        CODA => 3.6,
        DOWN_BOW => 1.27,
        UP_BOW => 1.98,
        ACCENT_ABOVE => 0.98,
        TENUTO_ABOVE => 0.2,
        MARCATO_ABOVE => 1.0,
        BREATH_MARK => 1.0,
        _ => 1.0,
    }
}

/// How far a glyph reaches above and below its baseline, in staff spaces.
pub fn extent(c: char) -> (f32, f32) {
    match c {
        G_CLEF => (4.39, 2.63),
        F_CLEF => (1.05, 2.54),
        C_CLEF => (2.02, 2.02),
        NOTEHEAD_DOUBLE_WHOLE => (0.62, 0.62),
        NOTEHEAD_WHOLE | NOTEHEAD_HALF | NOTEHEAD_BLACK => (0.5, 0.5),
        FLAT | DOUBLE_FLAT => (1.76, 0.7),
        NATURAL => (1.36, 1.34),
        SHARP => (1.4, 1.39),
        DOUBLE_SHARP => (0.51, 0.5),
        REST_WHOLE => (0.04, 0.54),
        REST_HALF => (0.57, 0.01),
        REST_QUARTER => (1.49, 1.5),
        REST_8TH => (0.7, 1.0),
        REST_16TH => (0.72, 2.0),
        REST_32ND => (1.7, 2.0),
        REST_64TH => (1.72, 3.01),
        c if FLAG_UP.contains(&c) => (0.6, 3.25),
        c if FLAG_DOWN.contains(&c) => (3.25, 0.7),
        TIME_SIG_COMMON | TIME_SIG_CUT | TIME_SIG_PLUS => (1.44, 1.44),
        AUGMENTATION_DOT | REPEAT_DOT => (0.2, 0.2),
        BRACKET_TOP => (1.18, 0.0),
        BRACKET_BOTTOM => (0.0, 1.18),
        FERMATA_BELOW => (0.0, 1.33),
        STACCATO_BELOW | ACCENT_BELOW | TENUTO_BELOW => (0.0, 0.98),
        DYN_P | DYN_M | DYN_F | DYN_R | DYN_S | DYN_Z => (1.78, 0.61),
        CODA => (3.6, 0.64),
        c if (TIME_SIG_DIGIT_0..TIME_SIG_DIGIT_0 + 10).contains(&(c as u32)) => (1.04, 1.04),
        c if (TUPLET_DIGIT_0..TUPLET_DIGIT_0 + 10).contains(&(c as u32)) => (1.5, 0.03),
        c => (height(c), 0.1),
    }
}

pub fn time_sig_digit(d: u32) -> char {
    char::from_u32(TIME_SIG_DIGIT_0 + d.min(9)).unwrap_or(TIME_SIG_PLUS)
}

pub fn tuplet_digit(d: u32) -> char {
    char::from_u32(TUPLET_DIGIT_0 + d.min(9)).unwrap_or('\u{E883}')
}

/// Path noteheads are drawn in design units (40 per staff space) around their centre.
pub const SHAPE_UNITS: f32 = 40.0;

/// The raised notehead ("corner-crossed rhomb"): a backward-leaning rhomb with
/// sharp corners, cut flat top and bottom so that in a space it keeps clear of
/// the staff lines, with four round-ended bar ends sticking out at its corners.
/// The ends run at the angles of the full rhomb's diagonals, each offset so its
/// centre line passes through a corner of the cut body. The black body is cut
/// at y = +-12; the half and whole at +-14, keeping their open middles large
/// enough to read as open at small sizes. All three rhombs have the same lean;
/// the black and half are 2 units narrower than the whole, whose sides are
/// thicker, and their stems sit at x = +-17. Every outline is half-turn symmetric,
/// so one glyph serves both stem directions. Counters wind opposite to their
/// outlines, so both the even-odd and non-zero fill rules work.
const RAISED_BLACK: &str = "M-34.6,-14.5L-18.7,-4.1L-14.3,6.7L-21.4,16.6C-23,18.8 -22.5,21.8 -20.3,23.4C-18.1,25 -15.1,24.5 -13.5,22.3L-6.1,12L13,12L29.3,22.6C31.5,24.1 34.6,23.5 36.1,21.2C37.5,19 36.9,15.9 34.6,14.5L18.7,4.1L14.3,-6.7L21.4,-16.6C23,-18.8 22.5,-21.8 20.3,-23.4C18.1,-25 15.1,-24.5 13.5,-22.3L6.1,-12L-13,-12L-29.3,-22.6C-31.5,-24.1 -34.6,-23.5 -36.1,-21.2C-37.5,-19 -36.9,-15.9 -34.6,-14.5Z";
const RAISED_OUTLINE: &str = "M-12.8,24.5L-5.3,14L13.8,14L30.1,24.6C32.3,26.1 35.4,25.5 36.9,23.3C38.4,21 37.7,18 35.5,16.5L19.5,6.1L13.4,-8.7L20.6,-18.6C22.2,-20.8 21.7,-23.8 19.6,-25.4C17.5,-27 14.5,-26.6 12.8,-24.5L5.3,-14L-13.8,-14L-30.1,-24.6C-32.3,-26.1 -35.4,-25.5 -36.9,-23.3C-38.4,-21 -37.7,-18 -35.5,-16.5L-19.5,-6.1L-13.4,8.7L-20.6,18.6C-22.2,20.8 -21.7,23.8 -19.6,25.4C-17.5,27 -14.5,26.6 -12.8,24.5Z";
const RAISED_HALF_COUNTER: &str = "M-7,8.5L-14,-8.5L7,-8.5L14,8.5Z";
const RAISED_WHOLE: &str = "M-37.5,-16.5L-21.5,-6.1L-15.4,8.7L-22.6,18.6C-24.2,20.8 -23.7,23.9 -21.5,25.4C-19.3,27 -16.2,26.5 -14.7,24.3L-7.3,14L15.8,14L32.1,24.6C34.4,26.1 37.4,25.5 38.9,23.2C40.4,20.9 39.7,17.9 37.5,16.5L21.5,6.1L15.4,-8.7L22.6,-18.6C24.2,-20.8 23.7,-23.9 21.5,-25.4C19.3,-27 16.2,-26.5 14.7,-24.3L7.3,-14L-15.8,-14L-32.1,-24.6C-34.4,-26.1 -37.4,-25.5 -38.9,-23.2C-40.4,-20.9 -39.7,-17.9 -37.5,-16.5ZM13.2,9L-5.8,9L-13.2,-9L5.8,-9L13.2,9Z";

impl crate::Shape {
    /// SVG path data in design units, centred on the origin.
    pub fn path(self) -> String {
        match self {
            crate::Shape::RaisedBlack => RAISED_BLACK.to_string(),
            crate::Shape::RaisedHalf => format!("{}{}", RAISED_OUTLINE, RAISED_HALF_COUNTER),
            crate::Shape::RaisedWhole => RAISED_WHOLE.to_string(),
        }
    }

    /// Half the width, in design units, including the bar ends.
    pub fn half_units(self) -> f32 {
        match self {
            crate::Shape::RaisedBlack => 36.9,
            crate::Shape::RaisedHalf => 37.7,
            crate::Shape::RaisedWhole => 39.7,
        }
    }

    /// Width in staff spaces.
    pub fn width(self) -> f32 {
        2.0 * self.half_units() / SHAPE_UNITS
    }

    /// Half the height in staff spaces, including the bar ends, which reach a
    /// little past the staff lines above and below. The bodies themselves are
    /// 12 (black) and 14 (half, whole) units high either side of the centre.
    pub fn half_height(self) -> f32 {
        match self {
            crate::Shape::RaisedBlack => 24.4 / SHAPE_UNITS,
            crate::Shape::RaisedHalf | crate::Shape::RaisedWhole => 26.4 / SHAPE_UNITS,
        }
    }

    /// x (design units, from the centre) of the up stem's right edge and the
    /// down stem's left edge: where the rhomb's side crosses the centre line.
    /// The whole has no stem; its value is given for completeness.
    pub fn stem_x_units(self) -> f32 {
        match self {
            crate::Shape::RaisedBlack | crate::Shape::RaisedHalf => 17.0,
            crate::Shape::RaisedWhole => 19.0,
        }
    }

    /// Distance from the left edge to the right edge of an up stem.
    pub fn stem_up_x(self) -> f32 {
        (self.half_units() + self.stem_x_units()) / SHAPE_UNITS
    }

    /// Distance from the left edge to the left edge of a down stem.
    pub fn stem_down_x(self) -> f32 {
        (self.half_units() - self.stem_x_units()) / SHAPE_UNITS
    }

    /// Vertical offset (staff spaces) at which the stem meets the head: raised
    /// heads take the stem on their centre line, where the rhomb's side is at
    /// exactly stem_x_units(). Use this in place of STEM_ATTACH_Y for raised heads.
    pub fn stem_attach_y(self) -> f32 {
        0.0
    }
}

// Bravura engraving defaults, in staff spaces.
pub const STAFF_LINE: f32 = 0.13;
pub const STEM: f32 = 0.12;
pub const STEM_LENGTH: f32 = 3.5;
pub const LEDGER_LINE: f32 = 0.16;
pub const LEDGER_EXTENSION: f32 = 0.4;
pub const BEAM: f32 = 0.5;
pub const BEAM_SPACING: f32 = 0.25;
pub const THIN_BARLINE: f32 = 0.16;
pub const THICK_BARLINE: f32 = 0.5;
pub const BARLINE_SEPARATION: f32 = 0.4;
pub const REPEAT_DOT_SEPARATION: f32 = 0.16;
pub const STEM_ATTACH_Y: f32 = 0.168;
pub const BRACKET: f32 = 0.12;
