//! The printf family's setting reads at a call site (ADR-0101 §3.2 and D4).
//!
//! `sprintf`, `printf`, `vsprintf` and `vprintf` are catalogued with two reads
//! ([`steins_catalog::effect_labels`]): the locale, which `%f`, `%g` and `%G` render the
//! decimal point of, and the `precision` ini, which a `%s` of a float renders through. Both
//! are **conditional on the call**: a `'%d'` format reads neither. A label is proven at a call
//! only where the read is unconditional for the call as written, so the row is the upper
//! bound this module narrows, and what it cannot narrow to a proof it names a gap
//! ([`GapKind::ValueDependentRead`]) instead of a label (ADR-0101 §3.2):
//!
//! * a **literal format** with no `f`/`g`/`G` drops the locale read, one with such a
//!   conversion keeps it, and a format that is no literal or that the parser cannot read is
//!   the gap, for the locale and the precision read alike;
//! * at a literal format, each `%s` value is a float, no float or neither
//!   ([`Frame::float_class`]). A value shown a float keeps the `precision` read, since the
//!   read happens on every run; values all shown no float drop it; any other value is the
//!   gap. A position the call does not supply renders nothing (`ArgumentCountError`), and a
//!   vector's elements are not read, so a `%s` over one is the gap.
//!
//! `printf` and `vprintf` keep `io.output.buffer`, which is unconditional.

use steins_catalog::{ArgReach, FormatReading, PrintfFamily};
use steins_syntax::{ArgShape, ConstArgs};

use super::GapKind;
use super::reach::{FloatClass, Frame, literal_format};
use crate::cx::Cx;

/// The locale cell's read, ADR-0101 §2.2.
const LOCALE: &str = "global.read.setting.locale";
/// The `precision` cell's read, ADR-0101 D4.
const PRECISION: &str = "global.read.setting.precision";

/// Narrow the row `labels` of a call to the builtin `name`, when it is a printf-family call,
/// by what the call's literal format and arguments show, and say whether a setting read is
/// left that depends on a value the call site cannot see.
pub(super) fn narrow_labels(
    cx: &Cx,
    frame: &Frame,
    name: &str,
    (shapes, consts): (Option<&[ArgShape]>, &ConstArgs),
    labels: &mut Vec<&'static str>,
) -> Option<GapKind> {
    let family = steins_catalog::printf_family(name)?;
    let Some(reading) = literal_format(consts, &family).and_then(steins_catalog::read_format)
    else {
        return unreadable_format(labels);
    };
    if !reading.reads_locale {
        labels.retain(|label| *label != LOCALE);
    }
    match precision_read(cx, frame, &family, &reading, (shapes, consts)) {
        PrecisionRead::Proven => None,
        PrecisionRead::Absent => {
            labels.retain(|label| *label != PRECISION);
            None
        }
        PrecisionRead::Depends => {
            labels.retain(|label| *label != PRECISION);
            Some(GapKind::ValueDependentRead)
        }
    }
}

/// The row of a printf-family builtin handed over as a callback, or called with a format the
/// parser cannot read: both reads depend on a format the site cannot see, so neither is a
/// label and the call is [`GapKind::ValueDependentRead`].
pub(super) fn unreadable_format(labels: &mut Vec<&'static str>) -> Option<GapKind> {
    labels.retain(|label| *label != LOCALE && *label != PRECISION);
    Some(GapKind::ValueDependentRead)
}

/// What a literal format and its arguments say of the `precision` read.
enum PrecisionRead {
    /// Some `%s` consumes a value shown to be a float.
    Proven,
    /// No `%s` consumes a value that is, or may be, a float.
    Absent,
    /// Some `%s` consumes a value that may or may not be a float.
    Depends,
}

/// The `precision` read of a literal format ([`PrecisionRead`]).
fn precision_read(
    cx: &Cx,
    frame: &Frame,
    family: &PrintfFamily,
    reading: &FormatReading,
    (shapes, consts): (Option<&[ArgShape]>, &ConstArgs),
) -> PrecisionRead {
    let rendered =
        reading.reach.iter().enumerate().filter(|(_, reach)| **reach == ArgReach::Object);
    if family.is_vector() {
        return if rendered.count() > 0 { PrecisionRead::Depends } else { PrecisionRead::Absent };
    }
    let Some(shapes) = shapes else { return PrecisionRead::Depends };
    let mut classes = Vec::new();
    for (value, _) in rendered {
        let position = family.format_position() + 1 + value;
        // A value the call does not supply is an `ArgumentCountError`, rendered never.
        if position >= shapes.len() {
            continue;
        }
        classes.push(
            match consts.float_evidence.iter().find(|(p, _)| usize::from(*p) == position) {
                Some((_, evidence)) => frame.float_class(cx, evidence),
                None => FloatClass::Unknown,
            },
        );
    }
    if classes.contains(&FloatClass::Yes) {
        PrecisionRead::Proven
    } else if classes.contains(&FloatClass::Unknown) {
        PrecisionRead::Depends
    } else {
        PrecisionRead::Absent
    }
}

#[cfg(test)]
mod tests {
    use super::{LOCALE, PRECISION};

    /// The labels this module drops are registry entries, spelled as the rows spell them.
    #[test]
    fn the_labels_are_the_rows_labels() {
        for label in [LOCALE, PRECISION] {
            assert!(steins_catalog::is_known_label(label), "{label}");
            assert!(steins_catalog::effect_labels("sprintf").is_some_and(|l| l.contains(&label)));
            assert!(steins_catalog::effect_labels("printf").is_some_and(|l| l.contains(&label)));
        }
    }
}
