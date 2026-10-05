//! Small JSON helpers. Wire names are spelled out here rather than taken from
//! `Debug`, so the format cannot change by accident.

use crate::Outcome;
use core::fmt;

pub(crate) fn quote(out: &mut impl fmt::Write, value: &str) -> fmt::Result {
    out.write_char('"')?;
    for ch in value.chars() {
        match ch {
            '"' => out.write_str("\\\"")?,
            '\\' => out.write_str("\\\\")?,
            '\n' => out.write_str("\\n")?,
            '\r' => out.write_str("\\r")?,
            '\t' => out.write_str("\\t")?,
            ch if ch < '\u{20}' => write!(out, "\\u{:04x}", ch as u32)?,
            ch => out.write_char(ch)?,
        }
    }
    out.write_char('"')
}

pub(crate) fn outcome_name(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Succeeded => "Succeeded",
        Outcome::Skipped => "Skipped",
        Outcome::Cancelled => "Cancelled",
        Outcome::Failed => "Failed",
        Outcome::Abandoned => "Abandoned",
        Outcome::NotRun => "NotRun",
        _ => "Other",
    }
}
