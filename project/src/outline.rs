//! A first pass over an untrusted `project.json` that keeps almost nothing: the header (`format`,
//! `version`), the channel ids validation keeps and which patterns have a `notes` list. It reads
//! the whole document without building it, so `load` can refuse a newer version before typing
//! anything, and the typed pass ([`crate::reading`]) knows the version and the channels whatever
//! order the fields come in.
//!
//! Every value of an unexpected type counts as absent here: the typed pass reports it. The one
//! exception is a channel or pattern written as an array of its fields: it is refused as the
//! typed pass refuses it ([`crate::reading::Named`]), so the two passes never read one file two
//! ways.

use grooviq_engine::MAX_CHANNELS;
use serde::Deserializer;
use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use std::collections::HashSet;
use std::fmt;
use std::io::Read;

/// What the first pass found.
#[derive(Debug, Default)]
pub(crate) struct Outline {
    /// `format`, if it is a string.
    pub(crate) format: Option<String>,
    /// `version`, if it is a non-negative integer.
    pub(crate) version: Option<u64>,
    /// The ids of the channels validation keeps: the first [`MAX_CHANNELS`] distinct ones.
    pub(crate) channels: HashSet<u32>,
    /// Whether each of `patterns`, in order, has a `notes` key: the patterns whose notes the
    /// typed pass reads, in the order it reads them.
    pub(crate) has_notes: Vec<bool>,
}

/// Reads `json` once: `None` if the document is not an object. A channel or pattern written as
/// an array is a `Data` error; broken JSON is a `Syntax` or `Eof` one.
pub(crate) fn read(json: impl Read) -> serde_json::Result<Option<Outline>> {
    read_mode(json, false)
}

pub(crate) fn read_header(json: impl Read) -> serde_json::Result<Option<Outline>> {
    read_mode(json, true)
}

fn read_mode(json: impl Read, header_only: bool) -> serde_json::Result<Option<Outline>> {
    let mut de = serde_json::Deserializer::from_reader(json);
    let outline = (&mut de).deserialize_any(OutlineVisitor(header_only))?;
    de.end()?;
    Ok(outline)
}

/// The refusal of a struct written as an array.
fn not_named<E: de::Error>() -> E {
    E::invalid_type(de::Unexpected::Seq, &crate::reading::NAMED)
}

/// The scalar visits of a lenient visitor: each one is "something else", `$default`.
macro_rules! anything_else {
    ($value:ty, $default:expr) => {
        fn visit_bool<E: de::Error>(self, _: bool) -> Result<$value, E> {
            Ok($default)
        }
        fn visit_i64<E: de::Error>(self, _: i64) -> Result<$value, E> {
            Ok($default)
        }
        fn visit_f64<E: de::Error>(self, _: f64) -> Result<$value, E> {
            Ok($default)
        }
        fn visit_unit<E: de::Error>(self) -> Result<$value, E> {
            Ok($default)
        }
    };
    ($value:ty, $default:expr, with numbers and text) => {
        anything_else!($value, $default);
        fn visit_u64<E: de::Error>(self, _: u64) -> Result<$value, E> {
            Ok($default)
        }
        fn visit_str<E: de::Error>(self, _: &str) -> Result<$value, E> {
            Ok($default)
        }
    };
}

/// Drains a list or an object without keeping anything.
fn drain_seq<'de, A: SeqAccess<'de>>(mut seq: A) -> Result<(), A::Error> {
    while seq.next_element::<IgnoredAny>()?.is_some() {}
    Ok(())
}

fn drain_map<'de, A: MapAccess<'de>>(mut map: A) -> Result<(), A::Error> {
    while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
    Ok(())
}

struct OutlineVisitor(bool);

impl<'de> Visitor<'de> for OutlineVisitor {
    type Value = Option<Outline>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any value")
    }

    anything_else!(Option<Outline>, None, with numbers and text);

    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Option<Outline>, A::Error> {
        drain_seq(seq)?;
        Ok(None)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Option<Outline>, A::Error> {
        let mut outline = Outline::default();
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "format" => outline.format = map.next_value_seed(Lenient(Header::Format))?.text,
                "version" => outline.version = map.next_value_seed(Lenient(Header::Version))?.int,
                "channels" if !self.0 => {
                    outline.channels = map.next_value_seed(Lenient(Channels))?
                }
                "patterns" if !self.0 => {
                    outline.has_notes = map.next_value_seed(Lenient(Patterns))?
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(Some(outline))
    }
}

/// Deserializes any value with `T`'s visitor, which accepts every type.
struct Lenient<T>(T);

impl<'de, T: Visitor<'de>> DeserializeSeed<'de> for Lenient<T> {
    type Value = T::Value;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<T::Value, D::Error> {
        d.deserialize_any(self.0)
    }
}

/// A header field: a string or a non-negative integer, anything else is absent.
enum Header {
    Format,
    Version,
}

#[derive(Default)]
struct Scalar {
    text: Option<String>,
    int: Option<u64>,
}

impl<'de> Visitor<'de> for Header {
    type Value = Scalar;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any value")
    }

    anything_else!(Scalar, Scalar::default());

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Scalar, E> {
        Ok(match self {
            Self::Format => Scalar {
                text: Some(v.to_owned()),
                int: None,
            },
            Self::Version => Scalar::default(),
        })
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Scalar, E> {
        Ok(Scalar {
            text: None,
            int: Some(v),
        })
    }

    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Scalar, A::Error> {
        drain_seq(seq)?;
        Ok(Scalar::default())
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Scalar, A::Error> {
        drain_map(map)?;
        Ok(Scalar::default())
    }
}

/// `channels`: the first [`MAX_CHANNELS`] distinct ids (later ones validation drops).
struct Channels;

impl<'de> Visitor<'de> for Channels {
    type Value = HashSet<u32>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any value")
    }

    anything_else!(HashSet<u32>, HashSet::new(), with numbers and text);

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<HashSet<u32>, A::Error> {
        let mut ids = HashSet::new();
        while let Some(id) = seq.next_element_seed(Lenient(Field("id")))? {
            if let Some(id) = id.and_then(|id| u32::try_from(id).ok())
                && ids.len() < MAX_CHANNELS
            {
                ids.insert(id);
            }
        }
        Ok(ids)
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<HashSet<u32>, A::Error> {
        drain_map(map)?;
        Ok(HashSet::new())
    }
}

/// One object's integer field `.0` (the last one, if repeated), or `None`.
struct Field(&'static str);

impl<'de> Visitor<'de> for Field {
    type Value = Option<u64>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any value")
    }

    anything_else!(Option<u64>, None, with numbers and text);

    fn visit_seq<A: SeqAccess<'de>>(self, _: A) -> Result<Option<u64>, A::Error> {
        Err(not_named())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Option<u64>, A::Error> {
        let mut value = None;
        while let Some(key) = map.next_key::<String>()? {
            if key == self.0 {
                value = map.next_value_seed(Lenient(Header::Version))?.int;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(value)
    }
}

/// `patterns`: per element, whether it has a `notes` key.
struct Patterns;

impl<'de> Visitor<'de> for Patterns {
    type Value = Vec<bool>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any value")
    }

    anything_else!(Vec<bool>, Vec::new(), with numbers and text);

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<bool>, A::Error> {
        let mut has_notes = Vec::new();
        while let Some(has) = seq.next_element_seed(Lenient(HasNotes))? {
            has_notes.push(has);
        }
        Ok(has_notes)
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Vec<bool>, A::Error> {
        drain_map(map)?;
        Ok(Vec::new())
    }
}

/// One pattern: whether it has a `notes` key (whatever its value).
struct HasNotes;

impl<'de> Visitor<'de> for HasNotes {
    type Value = bool;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any value")
    }

    anything_else!(bool, false, with numbers and text);

    fn visit_seq<A: SeqAccess<'de>>(self, _: A) -> Result<bool, A::Error> {
        Err(not_named())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<bool, A::Error> {
        let mut has = false;
        while let Some(key) = map.next_key::<String>()? {
            has |= key == "notes";
            map.next_value::<IgnoredAny>()?;
        }
        Ok(has)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outline(json: &str) -> serde_json::Result<Option<Outline>> {
        read(json.as_bytes())
    }

    #[test]
    fn channels_and_patterns_written_as_arrays_are_refused() {
        for json in [
            r#"{"channels":[[1]],"patterns":[]}"#,
            r#"{"channels":[{"id":1}],"patterns":[[0,"P",16,0,[]]]}"#,
        ] {
            let err = outline(json).unwrap_err();
            assert!(err.is_data(), "{json}: {err}");
            assert!(err.to_string().contains(crate::reading::NAMED), "{err}");
        }
        // Named fields are read; a document that is not an object is no outline.
        let o = outline(
            r#"{"format":"f","version":2,"channels":[{"id":1},{"id":1},{"id":4}],
                "patterns":[{"id":0},{"notes":[]}]}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(o.channels, HashSet::from([1, 4]));
        assert_eq!(o.has_notes, vec![false, true]);
        assert!(outline("[1, 2]").unwrap().is_none());
    }
}
