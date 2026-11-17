//! A first pass over an untrusted `project.json` that keeps almost nothing: the header (`format`,
//! `version`) and how many notes each pattern holds. It reads the whole document without building
//! it, so `load` can refuse a newer version before typing anything and can say how many notes past
//! the cap it skipped (the typed pass never holds them).
//!
//! Every value of an unexpected type counts as absent here: the typed pass reports it.

use serde::Deserializer;
use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use std::fmt;
use std::io::Read;

/// What the first pass found.
#[derive(Debug, Default)]
pub(crate) struct Outline {
    /// `format`, if it is a string.
    pub(crate) format: Option<String>,
    /// `version`, if it is a non-negative integer.
    pub(crate) version: Option<u64>,
    /// The length of each `patterns[i].notes` list, in pattern order (0 if absent).
    pub(crate) notes: Vec<usize>,
}

/// Reads `json` once. A document that is not an object is a `Data` error; broken JSON is a
/// `Syntax` or `Eof` one.
pub(crate) fn read(json: impl Read) -> serde_json::Result<Outline> {
    let mut de = serde_json::Deserializer::from_reader(json);
    let outline = (&mut de).deserialize_map(OutlineVisitor)?;
    de.end()?;
    Ok(outline)
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
fn drain_seq<'de, A: SeqAccess<'de>>(mut seq: A) -> Result<usize, A::Error> {
    let mut n = 0;
    while seq.next_element::<IgnoredAny>()?.is_some() {
        n += 1;
    }
    Ok(n)
}

fn drain_map<'de, A: MapAccess<'de>>(mut map: A) -> Result<(), A::Error> {
    while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
    Ok(())
}

struct OutlineVisitor;

impl<'de> Visitor<'de> for OutlineVisitor {
    type Value = Outline;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a project object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Outline, A::Error> {
        let mut outline = Outline::default();
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "format" => outline.format = map.next_value_seed(Lenient(Header::Format))?.text,
                "version" => outline.version = map.next_value_seed(Lenient(Header::Version))?.int,
                "patterns" => outline.notes = map.next_value_seed(Lenient(Patterns))?,
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(outline)
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

/// `patterns`: one note count per element.
struct Patterns;

impl<'de> Visitor<'de> for Patterns {
    type Value = Vec<usize>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any value")
    }

    anything_else!(Vec<usize>, Vec::new(), with numbers and text);

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<usize>, A::Error> {
        let mut counts = Vec::new();
        while let Some(n) = seq.next_element_seed(Lenient(PatternNotes))? {
            counts.push(n);
        }
        Ok(counts)
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Vec<usize>, A::Error> {
        drain_map(map)?;
        Ok(Vec::new())
    }
}

/// One pattern: the length of its `notes` list.
struct PatternNotes;

impl<'de> Visitor<'de> for PatternNotes {
    type Value = usize;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any value")
    }

    anything_else!(usize, 0, with numbers and text);

    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<usize, A::Error> {
        drain_seq(seq)?;
        Ok(0)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<usize, A::Error> {
        let mut notes = 0;
        while let Some(key) = map.next_key::<String>()? {
            if key == "notes" {
                notes = map.next_value_seed(Lenient(Count))?;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(notes)
    }
}

/// The length of a list; anything else is 0.
struct Count;

impl<'de> Visitor<'de> for Count {
    type Value = usize;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any value")
    }

    anything_else!(usize, 0, with numbers and text);

    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<usize, A::Error> {
        drain_seq(seq)
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<usize, A::Error> {
        drain_map(map)?;
        Ok(0)
    }
}
