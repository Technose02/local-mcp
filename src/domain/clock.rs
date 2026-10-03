//! Local date/time use case.
//!
//! LLMs frequently do not know the current date/time. This service reads the
//! system clock and the system time zone, combines them with optional region
//! clues (from the tool arguments and from the HTTP `Accept-Language` header),
//! and falls back to a configurable default zone when the clues are missing or
//! contradictory.

use std::fmt;

use jiff::Timestamp;
use jiff::tz::{Offset, TimeZone};

/// Region clues for one request.
#[derive(Debug, Clone, Default)]
pub struct TimeQuery {
    /// Explicit IANA name (`Europe/Berlin`) or UTC offset (`+02:00`).
    pub timezone: Option<String>,
    /// Region/locale hint supplied by the model, e.g. `de-DE`, `Germany`, `US`.
    pub locale: Option<String>,
    /// Locale hint gathered from the request, e.g. the `Accept-Language` header.
    pub request_locale: Option<String>,
}

/// Where the timezone in the answer came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeSource {
    Explicit,
    Argument,
    Request,
    System,
    Default,
}

impl TimeSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Argument => "argument",
            Self::Request => "request",
            Self::System => "system",
            Self::Default => "default",
        }
    }

    fn priority(self) -> u8 {
        match self {
            Self::Explicit => 4,
            Self::Argument => 3,
            Self::Request => 2,
            Self::System => 1,
            Self::Default => 0,
        }
    }
}

/// How confident we are that the chosen region is the user's region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Certainty {
    High,
    Medium,
    Low,
}

impl Certainty {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }
}

/// The resolved timezone plus an explanation for the model.
#[derive(Debug, Clone)]
pub struct ResolvedZone {
    pub timezone: String,
    pub source: TimeSource,
    pub certainty: Certainty,
    /// Short, low-key hint when the region was assumed or the clues conflicted.
    pub note: Option<String>,
    zone: TimeZone,
}

/// A ready-to-return date/time reading.
#[derive(Debug, Clone)]
pub struct TimeReport {
    pub datetime: String,
    pub date: String,
    pub time: String,
    pub weekday: String,
    pub timezone: String,
    pub utc_offset: String,
    pub unix_seconds: i64,
    pub utc: String,
    pub source: TimeSource,
    pub certainty: Certainty,
    pub note: Option<String>,
}

#[derive(Debug)]
pub enum TimeError {
    InvalidDefaultTimezone(String),
}

impl fmt::Display for TimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDefaultTimezone(zone) => {
                write!(f, "configured default timezone is invalid: '{zone}'")
            }
        }
    }
}

impl std::error::Error for TimeError {}

/// Business logic for reading the current local date/time.
pub struct TimeService {
    default_timezone: String,
}

impl TimeService {
    pub fn new(default_timezone: String) -> Self {
        Self { default_timezone }
    }

    /// Read the current time using the real system clock and system time zone.
    pub fn current(&self, query: TimeQuery) -> Result<TimeReport, TimeError> {
        let system = TimeZone::try_system().unwrap_or(TimeZone::UTC);
        self.current_at(Timestamp::now(), query, &system)
    }

    /// Read the time at a fixed instant in a given system zone (used by tests).
    pub fn current_at(
        &self,
        now: Timestamp,
        query: TimeQuery,
        system: &TimeZone,
    ) -> Result<TimeReport, TimeError> {
        if parse_zone(&self.default_timezone).is_none() {
            return Err(TimeError::InvalidDefaultTimezone(
                self.default_timezone.clone(),
            ));
        }
        let resolved = resolve_zone(&query, system, &self.default_timezone);
        Ok(build_report(now, &resolved))
    }
}

/// Resolve which timezone should be used for a request.
///
/// Priority: explicit argument > locale argument > request locale > system zone
/// > configured default. Contradictory clues are treated as ambiguous and fall
/// > back to the configured default with low certainty.
pub fn resolve_zone(query: &TimeQuery, system: &TimeZone, default_timezone: &str) -> ResolvedZone {
    let explicit = query
        .timezone
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let mut invalid_explicit = None;

    if let Some(raw) = explicit {
        match parse_zone(raw) {
            Some((zone, name)) => {
                return ResolvedZone {
                    timezone: name,
                    source: TimeSource::Explicit,
                    certainty: Certainty::High,
                    note: None,
                    zone,
                };
            }
            None => invalid_explicit = Some(raw.to_string()),
        }
    }

    let mut candidates: Vec<(TimeSource, String)> = Vec::new();
    if let Some(zone) = query.locale.as_deref().and_then(locale_to_zone) {
        candidates.push((TimeSource::Argument, zone));
    }
    if let Some(zone) = query.request_locale.as_deref().and_then(locale_to_zone) {
        candidates.push((TimeSource::Request, zone));
    }
    if let Some(name) = system.iana_name()
        && !is_generic_zone(name)
    {
        candidates.push((TimeSource::System, name.to_string()));
    }

    let mut distinct: Vec<String> = Vec::new();
    for (_, name) in &candidates {
        if !distinct.iter().any(|existing| existing == name) {
            distinct.push(name.clone());
        }
    }

    match distinct.len() {
        1 => {
            let name = &distinct[0];
            let source = candidates
                .iter()
                .filter(|(_, candidate)| candidate == name)
                .map(|(source, _)| *source)
                .max_by_key(|source| source.priority())
                .unwrap_or(TimeSource::System);
            let certainty = match source {
                TimeSource::Argument | TimeSource::Explicit => Certainty::High,
                TimeSource::Request | TimeSource::System => Certainty::Medium,
                TimeSource::Default => Certainty::Low,
            };
            match TimeZone::get(name) {
                Ok(zone) => ResolvedZone {
                    timezone: name.clone(),
                    source,
                    certainty,
                    note: None,
                    zone,
                },
                Err(_) => default_resolution(default_timezone),
            }
        }
        0 => default_resolution(default_timezone),
        _ => {
            let mut resolved = default_resolution(default_timezone);
            resolved.note = Some(format!(
                "region clues conflict ({}); assuming {}",
                distinct.join(", "),
                resolved.timezone
            ));
            resolved
        }
    }
    .with_invalid_explicit(invalid_explicit)
}

impl ResolvedZone {
    fn with_invalid_explicit(mut self, invalid: Option<String>) -> Self {
        if let Some(invalid) = invalid {
            let suffix = format!("unknown timezone '{invalid}' ignored");
            self.note = Some(match self.note {
                Some(note) => format!("{note}; {suffix}"),
                None => suffix,
            });
        }
        self
    }
}

fn default_resolution(default_timezone: &str) -> ResolvedZone {
    let (zone, name) =
        parse_zone(default_timezone).unwrap_or_else(|| (TimeZone::UTC, "UTC".to_string()));
    ResolvedZone {
        timezone: name.clone(),
        source: TimeSource::Default,
        certainty: Certainty::Low,
        note: Some(format!(
            "timezone assumed: {name} (no reliable region hint)"
        )),
        zone,
    }
}

/// Parse an IANA zone name or a UTC offset into a `TimeZone`.
fn parse_zone(input: &str) -> Option<(TimeZone, String)> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(zone) = TimeZone::get(trimmed) {
        let name = zone
            .iana_name()
            .map(str::to_string)
            .unwrap_or_else(|| trimmed.to_string());
        return Some((zone, name));
    }
    if let Some(offset) = parse_offset(trimmed) {
        return Some((TimeZone::fixed(offset), format_offset(offset)));
    }
    None
}

/// Normalize an offset to `+HH:MM` (jiff renders whole hours as `+02`).
fn format_offset(offset: Offset) -> String {
    let total = offset.seconds();
    let sign = if total < 0 { '-' } else { '+' };
    let absolute = total.abs();
    let hours = absolute / 3600;
    let minutes = (absolute % 3600) / 60;
    let seconds = absolute % 60;
    if seconds == 0 {
        format!("{sign}{hours:02}:{minutes:02}")
    } else {
        format!("{sign}{hours:02}:{minutes:02}:{seconds:02}")
    }
}

/// Parse `+HH:MM`, `+HHMM`, `+HH` (and `-` variants) into a jiff `Offset`.
fn parse_offset(input: &str) -> Option<Offset> {
    let (sign, rest) = match input.as_bytes().first()? {
        b'+' => (1, &input[1..]),
        b'-' => (-1, &input[1..]),
        _ => return None,
    };
    let digits: String = rest.chars().filter(|character| *character != ':').collect();
    if digits.is_empty() || digits.len() > 4 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let (hours, minutes) = match digits.len() {
        1 | 2 => (digits.parse::<i32>().ok()?, 0),
        3 => (
            digits[..1].parse::<i32>().ok()?,
            digits[1..].parse::<i32>().ok()?,
        ),
        4 => (
            digits[..2].parse::<i32>().ok()?,
            digits[2..].parse::<i32>().ok()?,
        ),
        _ => return None,
    };
    if hours > 23 || minutes > 59 {
        return None;
    }
    Offset::from_seconds(sign * (hours * 3600 + minutes * 60)).ok()
}

/// Whether a time zone name carries no regional information.
fn is_generic_zone(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(lower.as_str(), "utc" | "gmt" | "etc/utc" | "etc/gmt") || lower.starts_with("etc/")
}

/// Map a free-form locale/region clue to exactly one IANA zone, if unambiguous.
///
/// Returns `None` for unknown or ambiguous clues such as a bare `en` or `es`.
pub fn locale_to_zone(locale: &str) -> Option<String> {
    let normalized = locale.trim().replace('_', "-").to_ascii_lowercase();
    if normalized.is_empty() {
        return None;
    }
    if let Some(zone) = region_alias(&normalized) {
        return Some(zone.to_string());
    }
    if normalized.len() == 2
        && let Some(zone) = country_zone(&normalized)
    {
        return Some(zone.to_string());
    }
    let subtags: Vec<&str> = normalized.split('-').collect();
    let language = subtags.first().copied().unwrap_or("");
    for tag in subtags.iter().skip(1) {
        if let Some(zone) = country_zone(tag) {
            return Some(zone.to_string());
        }
    }
    language_zone(language).map(str::to_string)
}

/// Country names, common aliases and well-known cities.
fn region_alias(name: &str) -> Option<&'static str> {
    let zone = match name {
        "germany" | "deutschland" | "berlin" | "munich" | "münchen" => "Europe/Berlin",
        "austria" | "österreich" | "vienna" | "wien" => "Europe/Vienna",
        "switzerland" | "schweiz" | "zurich" | "zürich" => "Europe/Zurich",
        "france" | "paris" => "Europe/Paris",
        "spain" | "españa" | "madrid" => "Europe/Madrid",
        "italy" | "italia" | "rome" | "milan" => "Europe/Rome",
        "netherlands" | "holland" | "amsterdam" => "Europe/Amsterdam",
        "belgium" | "brussels" => "Europe/Brussels",
        "poland" | "warsaw" => "Europe/Warsaw",
        "portugal" | "lisbon" => "Europe/Lisbon",
        "sweden" | "stockholm" => "Europe/Stockholm",
        "norway" | "oslo" => "Europe/Oslo",
        "denmark" | "copenhagen" => "Europe/Copenhagen",
        "finland" | "helsinki" => "Europe/Helsinki",
        "ireland" | "dublin" => "Europe/Dublin",
        "uk" | "united kingdom" | "england" | "london" | "britain" | "great britain" => {
            "Europe/London"
        }
        "usa" | "united states" | "new york" | "washington" | "boston" | "miami" => {
            "America/New_York"
        }
        "california" | "los angeles" | "san francisco" | "seattle" => "America/Los_Angeles",
        "chicago" | "texas" | "houston" | "dallas" => "America/Chicago",
        "canada" | "toronto" => "America/Toronto",
        "mexico" | "mexico city" => "America/Mexico_City",
        "brazil" | "brasil" | "sao paulo" | "são paulo" | "rio de janeiro" => "America/Sao_Paulo",
        "australia" | "sydney" | "melbourne" => "Australia/Sydney",
        "new zealand" | "auckland" => "Pacific/Auckland",
        "india" | "mumbai" | "delhi" | "bangalore" | "bengaluru" => "Asia/Kolkata",
        "japan" | "tokyo" => "Asia/Tokyo",
        "south korea" | "korea" | "seoul" => "Asia/Seoul",
        "china" | "beijing" | "shanghai" => "Asia/Shanghai",
        "taiwan" | "taipei" => "Asia/Taipei",
        "hong kong" => "Asia/Hong_Kong",
        "singapore" => "Asia/Singapore",
        "russia" | "moscow" => "Europe/Moscow",
        "ukraine" | "kyiv" | "kiev" => "Europe/Kyiv",
        "turkey" | "türkiye" | "istanbul" => "Europe/Istanbul",
        "israel" | "jerusalem" | "tel aviv" => "Asia/Jerusalem",
        "uae" | "dubai" => "Asia/Dubai",
        "egypt" | "cairo" => "Africa/Cairo",
        "south africa" | "johannesburg" | "cape town" => "Africa/Johannesburg",
        "nigeria" | "lagos" => "Africa/Lagos",
        _ => return None,
    };
    Some(zone)
}

/// ISO 3166-1 alpha-2 country codes.
fn country_zone(code: &str) -> Option<&'static str> {
    let zone = match code {
        "de" => "Europe/Berlin",
        "at" => "Europe/Vienna",
        "ch" => "Europe/Zurich",
        "fr" => "Europe/Paris",
        "es" => "Europe/Madrid",
        "it" => "Europe/Rome",
        "nl" => "Europe/Amsterdam",
        "be" => "Europe/Brussels",
        "pl" => "Europe/Warsaw",
        "pt" => "Europe/Lisbon",
        "se" => "Europe/Stockholm",
        "no" => "Europe/Oslo",
        "dk" => "Europe/Copenhagen",
        "fi" => "Europe/Helsinki",
        "ie" => "Europe/Dublin",
        "gb" | "uk" => "Europe/London",
        "cz" => "Europe/Prague",
        "hu" => "Europe/Budapest",
        "gr" => "Europe/Athens",
        "ro" => "Europe/Bucharest",
        "us" => "America/New_York",
        "ca" => "America/Toronto",
        "mx" => "America/Mexico_City",
        "br" => "America/Sao_Paulo",
        "ar" => "America/Argentina/Buenos_Aires",
        "au" => "Australia/Sydney",
        "nz" => "Pacific/Auckland",
        "in" => "Asia/Kolkata",
        "jp" => "Asia/Tokyo",
        "kr" => "Asia/Seoul",
        "cn" => "Asia/Shanghai",
        "tw" => "Asia/Taipei",
        "hk" => "Asia/Hong_Kong",
        "sg" => "Asia/Singapore",
        "ru" => "Europe/Moscow",
        "ua" => "Europe/Kyiv",
        "tr" => "Europe/Istanbul",
        "il" => "Asia/Jerusalem",
        "sa" => "Asia/Riyadh",
        "ae" => "Asia/Dubai",
        "eg" => "Africa/Cairo",
        "za" => "Africa/Johannesburg",
        "ng" => "Africa/Lagos",
        "ke" => "Africa/Nairobi",
        "th" => "Asia/Bangkok",
        "vn" => "Asia/Ho_Chi_Minh",
        "id" => "Asia/Jakarta",
        "ph" => "Asia/Manila",
        "my" => "Asia/Kuala_Lumpur",
        "pk" => "Asia/Karachi",
        "bd" => "Asia/Dhaka",
        _ => return None,
    };
    Some(zone)
}

/// Language primary subtags, only where one zone clearly dominates.
fn language_zone(language: &str) -> Option<&'static str> {
    let zone = match language {
        "de" => "Europe/Berlin",
        "fr" => "Europe/Paris",
        "it" => "Europe/Rome",
        "nl" => "Europe/Amsterdam",
        "pl" => "Europe/Warsaw",
        "sv" => "Europe/Stockholm",
        "da" => "Europe/Copenhagen",
        "nb" | "nn" | "no" => "Europe/Oslo",
        "fi" => "Europe/Helsinki",
        "cs" => "Europe/Prague",
        "hu" => "Europe/Budapest",
        "el" => "Europe/Athens",
        "ro" => "Europe/Bucharest",
        "tr" => "Europe/Istanbul",
        "uk" => "Europe/Kyiv",
        "ja" => "Asia/Tokyo",
        "ko" => "Asia/Seoul",
        "he" => "Asia/Jerusalem",
        "hi" => "Asia/Kolkata",
        "th" => "Asia/Bangkok",
        "vi" => "Asia/Ho_Chi_Minh",
        "id" => "Asia/Jakarta",
        // Intentionally omitted because they are ambiguous without a region:
        // en, es, pt, zh, ar, ...
        _ => return None,
    };
    Some(zone)
}

fn build_report(now: Timestamp, resolved: &ResolvedZone) -> TimeReport {
    let zoned = now.to_zoned(resolved.zone.clone());
    let date = zoned.strftime("%Y-%m-%d").to_string();
    let time = zoned.strftime("%H:%M:%S").to_string();
    let weekday = zoned.strftime("%A").to_string();
    let offset = format_offset(zoned.offset());
    let datetime = format!("{date}T{time}{offset}");
    let utc = now.strftime("%Y-%m-%dT%H:%M:%SZ").to_string();

    TimeReport {
        datetime,
        date,
        time,
        weekday,
        timezone: resolved.timezone.clone(),
        utc_offset: offset,
        unix_seconds: now.as_second(),
        utc,
        source: resolved.source,
        certainty: resolved.certainty,
        note: resolved.note.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instant(value: &str) -> Timestamp {
        value.parse().expect("valid timestamp")
    }

    fn berlin() -> TimeZone {
        TimeZone::get("Europe/Berlin").expect("tz")
    }

    fn utc() -> TimeZone {
        TimeZone::get("UTC").expect("tz")
    }

    #[test]
    fn locale_mapping_is_unambiguous_only() {
        assert_eq!(locale_to_zone("de-DE").as_deref(), Some("Europe/Berlin"));
        assert_eq!(locale_to_zone("en-US").as_deref(), Some("America/New_York"));
        assert_eq!(locale_to_zone("en-GB").as_deref(), Some("Europe/London"));
        assert_eq!(
            locale_to_zone("pt-BR").as_deref(),
            Some("America/Sao_Paulo")
        );
        assert_eq!(locale_to_zone("Germany").as_deref(), Some("Europe/Berlin"));
        assert_eq!(
            locale_to_zone("zh-Hans-CN").as_deref(),
            Some("Asia/Shanghai")
        );
        // Bare 2-letter codes are read as ISO country codes.
        assert_eq!(locale_to_zone("es").as_deref(), Some("Europe/Madrid"));
        assert_eq!(locale_to_zone("pt").as_deref(), Some("Europe/Lisbon"));
        assert_eq!(locale_to_zone("en"), None);
        assert_eq!(locale_to_zone("xx"), None);
    }

    #[test]
    fn explicit_timezone_always_wins() {
        let query = TimeQuery {
            timezone: Some("America/New_York".to_string()),
            locale: Some("de-DE".to_string()),
            request_locale: None,
        };
        let resolved = resolve_zone(&query, &berlin(), "Europe/Berlin");
        assert_eq!(resolved.timezone, "America/New_York");
        assert_eq!(resolved.source, TimeSource::Explicit);
        assert_eq!(resolved.certainty, Certainty::High);
        assert!(resolved.note.is_none());
    }

    #[test]
    fn single_locale_argument_is_used() {
        let query = TimeQuery {
            locale: Some("en-US".to_string()),
            ..Default::default()
        };
        let resolved = resolve_zone(&query, &utc(), "Europe/Berlin");
        assert_eq!(resolved.timezone, "America/New_York");
        assert_eq!(resolved.source, TimeSource::Argument);
        assert_eq!(resolved.certainty, Certainty::High);
    }

    #[test]
    fn ambiguous_language_falls_back_to_default() {
        let query = TimeQuery {
            locale: Some("en".to_string()),
            ..Default::default()
        };
        let resolved = resolve_zone(&query, &utc(), "Europe/Berlin");
        assert_eq!(resolved.timezone, "Europe/Berlin");
        assert_eq!(resolved.source, TimeSource::Default);
        assert_eq!(resolved.certainty, Certainty::Low);
        assert!(resolved.note.is_some());
    }

    #[test]
    fn conflicting_clues_default_with_note() {
        let query = TimeQuery {
            locale: Some("en-US".to_string()),
            ..Default::default()
        };
        let resolved = resolve_zone(&query, &berlin(), "Europe/Berlin");
        assert_eq!(resolved.timezone, "Europe/Berlin");
        assert_eq!(resolved.source, TimeSource::Default);
        assert_eq!(resolved.certainty, Certainty::Low);
        assert!(resolved.note.unwrap().contains("conflict"));
    }

    #[test]
    fn named_system_zone_is_used() {
        let tokyo = TimeZone::get("Asia/Tokyo").expect("tz");
        let resolved = resolve_zone(&TimeQuery::default(), &tokyo, "Europe/Berlin");
        assert_eq!(resolved.timezone, "Asia/Tokyo");
        assert_eq!(resolved.source, TimeSource::System);
        assert_eq!(resolved.certainty, Certainty::Medium);
    }

    #[test]
    fn report_applies_dst_and_utc_details() {
        let service = TimeService::new("Europe/Berlin".to_string());
        let summer = service
            .current_at(
                instant("2026-07-01T10:00:00Z"),
                TimeQuery::default(),
                &utc(),
            )
            .expect("report");
        assert_eq!(summer.utc_offset, "+02:00");
        assert_eq!(summer.date, "2026-07-01");
        assert_eq!(summer.time, "12:00:00");
        assert_eq!(summer.datetime, "2026-07-01T12:00:00+02:00");
        assert_eq!(summer.utc, "2026-07-01T10:00:00Z");
        assert_eq!(summer.weekday, "Wednesday");
        assert_eq!(summer.source, TimeSource::Default);
        assert_eq!(summer.certainty, Certainty::Low);

        let winter = service
            .current_at(
                instant("2026-01-15T10:00:00Z"),
                TimeQuery::default(),
                &utc(),
            )
            .expect("report");
        assert_eq!(winter.utc_offset, "+01:00");
        assert_eq!(winter.datetime, "2026-01-15T11:00:00+01:00");
    }

    #[test]
    fn offset_argument_is_accepted() {
        let query = TimeQuery {
            timezone: Some("+05:30".to_string()),
            ..Default::default()
        };
        let resolved = resolve_zone(&query, &utc(), "Europe/Berlin");
        assert_eq!(resolved.source, TimeSource::Explicit);
        assert_eq!(resolved.certainty, Certainty::High);
        assert_eq!(resolved.timezone, "+05:30");
    }

    #[test]
    fn invalid_explicit_timezone_is_noted() {
        let query = TimeQuery {
            timezone: Some("Mars/Olympus".to_string()),
            ..Default::default()
        };
        let resolved = resolve_zone(&query, &utc(), "Europe/Berlin");
        assert_eq!(resolved.source, TimeSource::Default);
        assert!(resolved.note.unwrap().contains("Mars/Olympus"));
    }
}
