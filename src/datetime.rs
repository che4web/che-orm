#[cfg(feature = "sqlite")]
use time::{OffsetDateTime, UtcOffset};

/// SQL-only representation. JSON continues to use RFC 3339 independently.
#[cfg(feature = "sqlite")]
pub(crate) fn sqlite_datetime(value: OffsetDateTime) -> Result<String, crate::DecodeError> {
    let value = value.to_offset(UtcOffset::UTC);
    if !(0..=9999).contains(&value.year()) {
        return Err(crate::DecodeError::InvalidValue(
            "SQLite canonical datetime requires a UTC year between 0000 and 9999".into(),
        ));
    }
    Ok(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:09}Z",
        value.year(),
        u8::from(value.month()),
        value.day(),
        value.hour(),
        value.minute(),
        value.second(),
        value.nanosecond(),
    ))
}

pub(crate) const SQLITE_DATETIME_DEFAULT: &str =
    "(strftime('%Y-%m-%dT%H:%M:%f', 'now') || '000000Z')";

#[cfg(feature = "sqlite")]
pub(crate) fn normalize_sqlite_datetime(value: &str) -> Result<String, crate::DecodeError> {
    // RFC 3339 permits arbitrary fractional precision and leap seconds. Neither
    // can be represented losslessly by OffsetDateTime; reject rather than round.
    let fraction_digits = if value.as_bytes().get(19) == Some(&b'.') {
        value.as_bytes()[20..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count()
    } else {
        0
    };
    if fraction_digits > 9 || value.get(17..19) == Some("60") {
        return Err(crate::DecodeError::InvalidValue(
            "datetime normalization cannot represent leap seconds or more than nine fractional digits without rounding".into(),
        ));
    }
    let parsed = OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .map_err(|error| crate::DecodeError::InvalidValue(error.to_string()))?;
    sqlite_datetime(parsed)
}
