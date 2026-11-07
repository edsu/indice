//! Dates, numbers and sizes in the page language. The patterns and separators
//! live in the catalogs (`date-long`, `number-group`, `size-mb`, ...), so a
//! language is added without touching this file.

use super::{FluentArg, L10n};

impl L10n {
    /// A date written out, "7 January 2025", from a timestamp that starts with
    /// `YYYYMMDD` or `YYYY-MM-DD`; `None` for anything else.
    pub fn long_date(&self, ts: &str) -> Option<String> {
        let digits: String = ts.chars().filter(|c| c.is_ascii_digit()).take(8).collect();
        if digits.len() < 8 {
            return None;
        }
        let month: u32 = digits[4..6].parse().ok()?;
        let day: u32 = digits[6..8].parse().ok()?;
        if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
            return None;
        }
        Some(self.tr_args(
            "date-long",
            &[
                ("day", FluentArg::Str(&day.to_string())),
                ("month", FluentArg::Str(&self.month_name(month))),
                ("year", FluentArg::Str(&digits[..4])),
            ],
        ))
    }

    /// "May 2021". `month` is 1 to 12.
    pub fn month_year(&self, month: u32, year: i32) -> String {
        self.tr_args(
            "month-year",
            &[
                ("month", FluentArg::Str(&self.month_name(month))),
                ("year", FluentArg::Str(&year.to_string())),
            ],
        )
    }

    /// `23800` as `23,800` (English) or `23.800` (Italian).
    pub fn number(&self, n: u64) -> String {
        let digits = n.to_string();
        let group = self.tr("number-group");
        let mut out = String::new();
        for (i, c) in digits.chars().enumerate() {
            if i > 0 && (digits.len() - i).is_multiple_of(3) {
                out.push_str(&group);
            }
            out.push(c);
        }
        out
    }

    /// A byte count in 1024 steps with one decimal, "48.2 MB" or "48,2 MB".
    pub fn size(&self, bytes: u64) -> String {
        const UNITS: [&str; 5] = ["size-b", "size-kb", "size-mb", "size-gb", "size-tb"];
        let mut b = bytes as f64;
        let mut i = 0;
        while b >= 1024.0 && i < UNITS.len() - 1 {
            b /= 1024.0;
            i += 1;
        }
        let unit = self.tr(UNITS[i]);
        if i == 0 {
            format!("{bytes} {unit}")
        } else {
            let value = format!("{b:.1}").replace('.', &self.tr("number-decimal"));
            format!("{value} {unit}")
        }
    }

    fn month_name(&self, month: u32) -> String {
        self.tr(&format!("month-{month}"))
    }
}
