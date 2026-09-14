// Canonical wire format for datetimes, per docs/wire-format.md
// (docs/wire-format.md):
//
//   encode: exactly 6 fractional digits, Z suffix — 2026-03-17T14:30:00.123456Z
//   decode: generous — accept 0, 3, or 6 fractional digits, Z or +00:00
//
// Every field that crosses an implementation boundary uses this module.

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{self, Deserialize, Deserializer, Serializer};

pub fn to_wire(dt: &DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(SecondsFormat::Micros, true)
}

pub fn from_wire(s: &str) -> Result<DateTime<Utc>, chrono::ParseError> {
    DateTime::parse_from_rfc3339(s).map(|dt| dt.with_timezone(&Utc))
}

pub fn serialize<S: Serializer>(dt: &DateTime<Utc>, ser: S) -> Result<S::Ok, S::Error> {
    ser.serialize_str(&to_wire(dt))
}

pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<DateTime<Utc>, D::Error> {
    let s = String::deserialize(de)?;
    from_wire(&s).map_err(serde::de::Error::custom)
}

/// Same contract for `Option<DateTime<Utc>>` fields (null on the wire, never absent).
pub mod option {
    use super::*;

    pub fn serialize<S: Serializer>(dt: &Option<DateTime<Utc>>, ser: S) -> Result<S::Ok, S::Error> {
        match dt {
            Some(dt) => ser.serialize_some(&to_wire(dt)),
            None => ser.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        de: D,
    ) -> Result<Option<DateTime<Utc>>, D::Error> {
        let s: Option<String> = Option::deserialize(de)?;
        s.map(|s| from_wire(&s).map_err(serde::de::Error::custom))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_six_fractional_digits_with_z() {
        let dt = from_wire("2026-03-17T14:30:00.123456Z").unwrap();
        assert_eq!(to_wire(&dt), "2026-03-17T14:30:00.123456Z");
    }

    #[test]
    fn pads_short_fractions_to_six_digits() {
        let dt = from_wire("2026-03-17T14:30:00Z").unwrap();
        assert_eq!(to_wire(&dt), "2026-03-17T14:30:00.000000Z");
    }

    // The generous-decode table from the interop guide: every producer
    // in the wild must parse (Python 6-digit, JS/Swift 3-digit, bare, offset).
    #[test]
    fn decodes_all_known_producer_variants() {
        for s in [
            "2026-03-17T14:30:00Z",
            "2026-03-17T14:30:00.123Z",
            "2026-03-17T14:30:00.123456Z",
            "2026-03-17T14:30:00.123456+00:00",
            "2026-03-17T15:30:00.123456+01:00",
        ] {
            assert!(from_wire(s).is_ok(), "must accept {s}");
        }
    }

    #[test]
    fn rejects_non_datetime_garbage() {
        for s in ["", "yesterday", "2026-03-17", "14:30:00"] {
            assert!(from_wire(s).is_err(), "must reject {s:?}");
        }
    }

    // The SHARED generous-decode fixture: the Swift decode tests iterate the
    // exact same file, so a divergence between the two implementations on any
    // producer variant fails here (or there) rather than escaping to interop.
    #[test]
    fn shared_datetime_variants_decode_to_canonical() {
        let raw = include_str!("../../../tests/fixtures/datetime-variants.json");
        let v: serde_json::Value = serde_json::from_str(raw).unwrap();
        for row in v["accept"].as_array().unwrap() {
            let input = row["input"].as_str().unwrap();
            let expected = row["canonical"].as_str().unwrap();
            let dt = from_wire(input).unwrap_or_else(|_| panic!("must accept {input:?}"));
            assert_eq!(to_wire(&dt), expected, "canonical mismatch for {input:?}");
        }
        for bad in v["reject"].as_array().unwrap() {
            let s = bad.as_str().unwrap();
            assert!(from_wire(s).is_err(), "must reject {s:?}");
        }
    }
}
