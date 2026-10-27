//! New values for PL/SQL scalars, checked before the JDWP agent gets them.
//!
//! The agent parses a scalar's new `_value` text with little checking of
//! its own: a NUMBER beyond its precision or a RAW that isn't hex ends the
//! target session (ORA-03113), a PLS_INTEGER that isn't a number ends the
//! call, NUMBER `1e130` is stored as `~`, and a DATE it can't parse is set
//! to year 0 although it reports the failure. So only the types known here
//! can be changed, numbers are checked against their precision and range
//! first, and what can't be checked (dates in the session's NLS format,
//! string lengths) is left to the agent, which refuses those cleanly.

/// What the agent tells about a scalar's declaration (its `_canBeNull`,
/// `_precision`, … fields).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Constraints {
    pub can_be_null: bool,
    /// NUMBER(p, s); 0 = no precision.
    pub precision: i32,
    pub scale: i32,
    /// PLS_INTEGER subtypes (NATURAL, POSITIVE, …); both 0 = none.
    pub min: i32,
    pub max: i32,
    /// VARCHAR2 and RAW (`_maxLength`) or CHAR (`_length`).
    pub length: i32,
}

impl Default for Constraints {
    fn default() -> Self {
        Self {
            can_be_null: true,
            precision: 0,
            scale: 0,
            min: 0,
            max: 0,
            length: 0,
        }
    }
}

/// Whether values of this type (as the debugger names it) can be changed.
pub fn changeable(type_name: &str) -> bool {
    kind(type_name).is_some()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Number,
    Integer,
    BinaryFloat,
    BinaryDouble,
    Text,
    Boolean,
    /// Dates, timestamps and intervals: in the session's NLS format.
    DateTime,
    Raw,
}

fn kind(type_name: &str) -> Option<Kind> {
    Some(match type_name {
        "NUMBER" => Kind::Number,
        // Also BINARY_INTEGER, SIMPLE_INTEGER, NATURAL, POSITIVE, …
        "PLS_INTEGER" => Kind::Integer,
        "BINARY_FLOAT" => Kind::BinaryFloat,
        "BINARY_DOUBLE" => Kind::BinaryDouble,
        "VARCHAR2" | "NVARCHAR2" | "CHAR" | "NCHAR" => Kind::Text,
        "BOOLEAN" => Kind::Boolean,
        "DATE"
        | "TIMESTAMP"
        | "TIMESTAMP_WITH_TIMEZONE"
        | "TIMESTAMP_WITH_LOCAL_TIMEZONE"
        | "INTERVAL_YEAR_TO_MONTH"
        | "INTERVAL_DAY_TO_SECOND" => Kind::DateTime,
        "RAW" => Kind::Raw,
        _ => return None,
    })
}

/// The texts to write for `value` (`None` = NULL), to try in this order:
/// numbers with a decimal point and with a comma, as the session's
/// NLS_NUMERIC_CHARACTERS may want either (the agent refuses the other
/// cleanly). `current` is the value now, as the agent shows it.
pub fn prepare(
    type_name: &str,
    c: &Constraints,
    value: Option<&str>,
    current: Option<&str>,
) -> Result<Vec<Option<String>>, String> {
    let Some(kind) = kind(type_name) else {
        return Err(if type_name.ends_with("LOB") || type_name == "BFILE" {
            "LOBs can't be changed in the debugger".into()
        } else if type_name == "OPAQUE" {
            "its contents aren't available to the debugger".into()
        } else {
            format!("values of type {type_name} can't be changed in the debugger")
        });
    };
    // An empty string is NULL in Oracle.
    let value = match value {
        Some(v) if kind == Kind::Text => Some(v).filter(|v| !v.is_empty()),
        Some(v) => Some(v.trim()).filter(|v| !v.is_empty()),
        None => None,
    };
    let Some(value) = value else {
        return if c.can_be_null {
            Ok(vec![None])
        } else {
            Err("it can't be NULL (NOT NULL)".into())
        };
    };
    // The decimal mark the session shows first, if the value has one.
    let comma_first = current.is_some_and(|v| v.contains(',') && !v.contains('.'));
    let marks = if comma_first { [',', '.'] } else { ['.', ','] };
    let number = || Decimal::parse(value).ok_or_else(|| format!("{value} isn't a number"));
    let texts = match kind {
        Kind::Number => {
            let mut n = number()?;
            if n.point > MAX_NUMBER_POINT {
                return Err(format!("{value} is too large for a NUMBER"));
            }
            if n.point < MIN_NUMBER_POINT {
                n = Decimal::ZERO;
            }
            if c.precision > 0 {
                let rounded = n.round(c.scale);
                if !rounded.is_zero() && rounded.point > c.precision - c.scale {
                    return Err(format!(
                        "{value} is too large for {}",
                        number_type(c.precision, c.scale)
                    ));
                }
            }
            if n.is_integer() {
                vec![n.plain('.')]
            } else {
                marks.iter().map(|m| n.plain(*m)).collect()
            }
        }
        Kind::Integer => {
            // PL/SQL rounds to the nearest integer on assignment.
            let n = number()?.round(0);
            let i = n
                .to_i64()
                .filter(|i| i32::try_from(*i).is_ok())
                .ok_or_else(|| format!("{value} is out of range for PLS_INTEGER"))?;
            if (c.min, c.max) != (0, 0) && !(i64::from(c.min)..=i64::from(c.max)).contains(&i) {
                return Err(format!("{value} is out of range ({} to {})", c.min, c.max));
            }
            vec![i.to_string()]
        }
        Kind::BinaryFloat | Kind::BinaryDouble => {
            let n = number()?;
            let f: f64 = n
                .scientific('.')
                .parse()
                .map_err(|_| format!("{value} isn't a number"))?;
            let max = if kind == Kind::BinaryFloat {
                f64::from(f32::MAX)
            } else {
                f64::MAX
            };
            if !f.is_finite() || f.abs() > max {
                return Err(format!("{value} is out of range for {type_name}"));
            }
            marks.iter().map(|m| n.scientific(*m)).collect()
        }
        Kind::Text | Kind::DateTime => vec![value.to_owned()],
        Kind::Boolean => {
            let b = if value.eq_ignore_ascii_case("true") {
                "true"
            } else if value.eq_ignore_ascii_case("false") {
                "false"
            } else {
                return Err(format!("{value} isn't TRUE or FALSE"));
            };
            vec![b.to_owned()]
        }
        Kind::Raw => {
            if !value.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(format!("{value} isn't hexadecimal"));
            }
            let hex = if value.len() % 2 == 1 {
                format!("0{value}")
            } else {
                value.to_owned()
            };
            if c.length > 0 && hex.len() / 2 > c.length as usize {
                return Err(format!(
                    "{} bytes are too long for RAW({})",
                    hex.len() / 2,
                    c.length
                ));
            }
            vec![hex.to_uppercase()]
        }
    };
    Ok(texts.into_iter().map(Some).collect())
}

/// Why the agent refused a value (its JDWP error code).
pub fn refusal(type_name: &str, c: &Constraints, code: u16, current: Option<&str>) -> String {
    match (code, kind(type_name)) {
        // ACCESS_DENIED
        (111, _) => "it can't be changed (IN parameters are read-only)".into(),
        (103, Some(Kind::DateTime)) => {
            let what = type_name.replace('_', " ").to_lowercase();
            match current {
                Some(example) => format!("not a {what} in the session's format, e.g. {example}"),
                None => format!("not a {what} in the session's format (NLS settings)"),
            }
        }
        (103, _) => format!("not a valid {type_name} value"),
        (506, Some(Kind::Text)) if c.length > 0 => {
            format!("too long for {type_name}({})", c.length)
        }
        (506, _) => format!("not a valid {type_name} value"),
        (code, _) => format!("refused ({})", crate::jdwp::error_name(code)),
    }
}

fn number_type(precision: i32, scale: i32) -> String {
    if scale == 0 {
        format!("NUMBER({precision})")
    } else {
        format!("NUMBER({precision},{scale})")
    }
}

/// NUMBER holds less than 10^126 …
const MAX_NUMBER_POINT: i32 = 126;
/// … and rounds what is below 10^-130 to 0.
const MIN_NUMBER_POINT: i32 = -129;

/// A decimal number `±0.DIGITS × 10^point`, without leading or trailing
/// zeros in `digits` (none for zero).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Decimal {
    negative: bool,
    digits: Vec<u8>,
    point: i32,
}

impl Decimal {
    const ZERO: Decimal = Decimal {
        negative: false,
        digits: Vec::new(),
        point: 0,
    };

    /// `-1.5`, `+2`, `.5`, `1,5`, `1e3`, `1.5E-7`: one decimal mark (point
    /// or comma), no group separators.
    fn parse(text: &str) -> Option<Self> {
        let mut chars = text.trim().chars().peekable();
        let negative = match chars.peek() {
            Some('-') => {
                chars.next();
                true
            }
            Some('+') => {
                chars.next();
                false
            }
            _ => false,
        };
        let (mut int, mut frac) = (Vec::new(), Vec::new());
        let mut seen_mark = false;
        while let Some(&c) = chars.peek() {
            match c {
                '0'..='9' if seen_mark => frac.push(c as u8 - b'0'),
                '0'..='9' => int.push(c as u8 - b'0'),
                '.' | ',' if !seen_mark => seen_mark = true,
                _ => break,
            }
            chars.next();
        }
        if int.is_empty() && frac.is_empty() {
            return None;
        }
        let mut exponent: i64 = 0;
        if chars.next_if(|c| matches!(c, 'e' | 'E')).is_some() {
            let sign = match chars.next_if(|c| matches!(c, '+' | '-')) {
                Some('-') => -1,
                _ => 1,
            };
            let mut digits = String::new();
            while let Some(d) = chars.next_if(char::is_ascii_digit) {
                digits.push(d);
            }
            if digits.is_empty() || digits.len() > 6 {
                return None;
            }
            exponent = sign * digits.parse::<i64>().ok()?;
        }
        if chars.next().is_some() {
            return None;
        }
        let mut point = int.len() as i64 + exponent;
        let mut digits = int;
        digits.extend(frac);
        let leading = digits.iter().take_while(|d| **d == 0).count();
        digits.drain(..leading);
        point -= leading as i64;
        while digits.last() == Some(&0) {
            digits.pop();
        }
        if digits.is_empty() {
            return Some(Self::ZERO);
        }
        Some(Self {
            negative,
            digits,
            point: i32::try_from(point).ok()?,
        })
    }

    fn is_zero(&self) -> bool {
        self.digits.is_empty()
    }

    fn is_integer(&self) -> bool {
        self.digits.len() as i32 <= self.point || self.is_zero()
    }

    /// Rounded half away from zero to `places` decimals (negative: to tens,
    /// hundreds, …), as Oracle rounds on assignment.
    fn round(&self, places: i32) -> Self {
        let keep = i64::from(self.point) + i64::from(places);
        if keep >= self.digits.len() as i64 {
            return self.clone();
        }
        if keep < 0 {
            return Self::ZERO;
        }
        let keep = keep as usize;
        let mut digits = self.digits[..keep].to_vec();
        let mut point = self.point;
        if self.digits[keep] >= 5 {
            let mut i = digits.len();
            loop {
                if i == 0 {
                    digits.insert(0, 1);
                    point += 1;
                    break;
                }
                i -= 1;
                if digits[i] == 9 {
                    digits[i] = 0;
                } else {
                    digits[i] += 1;
                    break;
                }
            }
        }
        while digits.last() == Some(&0) {
            digits.pop();
        }
        if digits.is_empty() {
            return Self::ZERO;
        }
        Self {
            negative: self.negative,
            digits,
            point,
        }
    }

    fn to_i64(&self) -> Option<i64> {
        if !self.is_integer() || self.point > 18 {
            return None;
        }
        self.plain('.').parse().ok()
    }

    fn sign(&self) -> &'static str {
        if self.negative && !self.is_zero() {
            "-"
        } else {
            ""
        }
    }

    /// Without exponent, e.g. `-0.05` or `1500`.
    fn plain(&self, mark: char) -> String {
        if self.is_zero() {
            return "0".into();
        }
        let digits: String = self.digits.iter().map(|d| char::from(b'0' + d)).collect();
        let point = self.point;
        let body = if point <= 0 {
            format!("0{mark}{}{digits}", "0".repeat(-point as usize))
        } else if point as usize >= digits.len() {
            format!("{digits}{}", "0".repeat(point as usize - digits.len()))
        } else {
            let (int, frac) = digits.split_at(point as usize);
            format!("{int}{mark}{frac}")
        };
        format!("{}{body}", self.sign())
    }

    /// `1.5E+3`, `2E-7`.
    fn scientific(&self, mark: char) -> String {
        if self.is_zero() {
            return "0".into();
        }
        let digits: String = self.digits.iter().map(|d| char::from(b'0' + d)).collect();
        let (first, rest) = digits.split_at(1);
        let exponent = self.point - 1;
        let fraction = if rest.is_empty() {
            String::new()
        } else {
            format!("{mark}{rest}")
        };
        let sign = if exponent < 0 { '-' } else { '+' };
        format!("{}{first}{fraction}E{sign}{}", self.sign(), exponent.abs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn number(precision: i32, scale: i32) -> Constraints {
        Constraints {
            precision,
            scale,
            ..Default::default()
        }
    }

    fn texts(type_name: &str, c: &Constraints, value: &str) -> Result<Vec<String>, String> {
        prepare(type_name, c, Some(value), None).map(|t| {
            t.into_iter()
                .map(|t| t.unwrap_or_else(|| "NULL".into()))
                .collect()
        })
    }

    #[test]
    fn decimals() {
        let d = |t: &str| Decimal::parse(t).map(|d| d.plain('.'));
        assert_eq!(d("42"), Some("42".into()));
        assert_eq!(d(" -1.50 "), Some("-1.5".into()));
        assert_eq!(d("+.5"), Some("0.5".into()));
        assert_eq!(d("1,25"), Some("1.25".into()));
        assert_eq!(d("1e3"), Some("1000".into()));
        assert_eq!(d("-1.5E-3"), Some("-0.0015".into()));
        assert_eq!(d("000.000"), Some("0".into()));
        assert_eq!(d("-0"), Some("0".into()));
        for bad in [
            "", "-", ".", "1.2.3", "1,000.5", "abc", "1e", "1e+", "12a", "1 2", "1e3x",
        ] {
            assert_eq!(d(bad), None, "{bad}");
        }
        let r = |t: &str, places| Decimal::parse(t).unwrap().round(places).plain('.');
        assert_eq!(r("1.25", 1), "1.3");
        assert_eq!(r("-1.25", 1), "-1.3");
        assert_eq!(r("999.995", 2), "1000");
        assert_eq!(r("0.4", 0), "0");
        assert_eq!(r("0.5", 0), "1");
        assert_eq!(r("1250", -2), "1300");
        assert_eq!(r("0.0004", 3), "0");
        let s = |t: &str| Decimal::parse(t).unwrap().scientific(',');
        assert_eq!(s("1500"), "1,5E+3");
        assert_eq!(s("-0.0002"), "-2E-4");
    }

    #[test]
    fn numbers_are_checked_against_precision_and_range() {
        let any = Constraints::default();
        assert_eq!(texts("NUMBER", &any, "42"), Ok(vec!["42".into()]));
        assert_eq!(
            texts("NUMBER", &any, "1,5"),
            Ok(vec!["1.5".into(), "1,5".into()]),
            "a point first, then a comma"
        );
        assert_eq!(
            prepare("NUMBER", &any, Some("1.5"), Some("2,5")),
            Ok(vec![Some("1,5".into()), Some("1.5".into())]),
            "the session shows a comma"
        );
        assert_eq!(texts("NUMBER", &any, "-1.5e3"), Ok(vec!["-1500".into()]));
        assert!(texts("NUMBER", &any, "1e126").is_err(), "stored as ~");
        assert_eq!(texts("NUMBER", &any, "1e-200"), Ok(vec!["0".into()]));
        assert!(texts("NUMBER", &any, "abc").is_err());
        // NUMBER(5,2) takes up to 999.99, rounded to two decimals.
        let n52 = number(5, 2);
        assert!(texts("NUMBER", &n52, "999.994").is_ok());
        assert!(texts("NUMBER", &n52, "123.456").is_ok(), "rounded");
        assert_eq!(
            texts("NUMBER", &n52, "1234.5"),
            Err("1234.5 is too large for NUMBER(5,2)".into()),
            "would end the session"
        );
        assert!(texts("NUMBER", &n52, "999.995").is_err(), "rounds to 1000");
        assert!(texts("NUMBER", &number(3, 0), "999.4").is_ok());
        assert!(texts("NUMBER", &number(3, 0), "999.5").is_err());
        assert!(texts("NUMBER", &number(2, -2), "9949").is_ok());
        assert!(texts("NUMBER", &number(2, -2), "9950").is_err());
    }

    #[test]
    fn integers_are_checked_against_their_range() {
        let any = Constraints::default();
        assert_eq!(texts("PLS_INTEGER", &any, "42"), Ok(vec!["42".into()]));
        assert_eq!(texts("PLS_INTEGER", &any, "1.7"), Ok(vec!["2".into()]));
        assert_eq!(
            texts("PLS_INTEGER", &any, "-2147483648"),
            Ok(vec!["-2147483648".into()])
        );
        assert!(
            texts("PLS_INTEGER", &any, "2147483648").is_err(),
            "ends the call"
        );
        assert!(texts("PLS_INTEGER", &any, "abc").is_err(), "ends the call");
        let natural = Constraints {
            min: 0,
            max: i32::MAX,
            ..Default::default()
        };
        assert_eq!(
            texts("PLS_INTEGER", &natural, "-1"),
            Err("-1 is out of range (0 to 2147483647)".into())
        );
    }

    #[test]
    fn other_types() {
        let any = Constraints::default();
        assert_eq!(texts("BOOLEAN", &any, " False "), Ok(vec!["false".into()]));
        assert!(texts("BOOLEAN", &any, "maybe").is_err());
        assert_eq!(
            texts("BINARY_DOUBLE", &any, "1500"),
            Ok(vec!["1.5E+3".into(), "1,5E+3".into()])
        );
        assert!(texts("BINARY_FLOAT", &any, "1e39").is_err());
        assert!(texts("BINARY_DOUBLE", &any, "1e39").is_ok());
        let raw = Constraints {
            length: 2,
            ..Default::default()
        };
        assert_eq!(texts("RAW", &raw, "aff"), Ok(vec!["0AFF".into()]));
        assert!(texts("RAW", &raw, "ZZ").is_err(), "ends the session");
        assert!(texts("RAW", &raw, "010203").is_err());
        // Strings keep their spaces; the agent checks the length.
        assert_eq!(texts("VARCHAR2", &any, " a b "), Ok(vec![" a b ".into()]));
        assert_eq!(
            texts("DATE", &any, " 31-JAN-25 "),
            Ok(vec!["31-JAN-25".into()])
        );
        assert!(texts("CLOB", &any, "x").unwrap_err().contains("LOB"));
        assert!(texts("T_REC", &any, "x").is_err());
        assert!(!changeable("BLOB") && changeable("TIMESTAMP_WITH_TIMEZONE"));
    }

    #[test]
    fn null() {
        let any = Constraints::default();
        assert_eq!(prepare("NUMBER", &any, None, Some("1")), Ok(vec![None]));
        assert_eq!(
            texts("VARCHAR2", &any, ""),
            Ok(vec!["NULL".into()]),
            "'' is NULL"
        );
        assert_eq!(texts("NUMBER", &any, "  "), Ok(vec!["NULL".into()]));
        let not_null = Constraints {
            can_be_null: false,
            ..Default::default()
        };
        assert!(prepare("PLS_INTEGER", &not_null, None, Some("1")).is_err());
    }

    #[test]
    fn refusals() {
        let c = Constraints {
            length: 5,
            ..Default::default()
        };
        assert_eq!(
            refusal("VARCHAR2", &c, 506, None),
            "too long for VARCHAR2(5)"
        );
        assert_eq!(
            refusal("DATE", &c, 103, Some("29-FEB-24 12.00.00 AM")),
            "not a date in the session's format, e.g. 29-FEB-24 12.00.00 AM"
        );
        assert_eq!(
            refusal("NUMBER", &c, 111, Some("3")),
            "it can't be changed (IN parameters are read-only)"
        );
    }
}
