pub mod api;
pub mod parse_file;
pub mod types;

use std::collections::{HashMap, HashSet};
use tracing::error;

pub use api::*;

#[derive(Debug, Clone)]
pub struct Note {
    pub id: Option<usize>,
    pub deck: String,
    pub model: String,
    pub fields: HashMap<String, String>,
    pub tags: Vec<String>,
    // just for error messages
    pub question: Option<String>,
}

impl Note {
    pub fn question_or_fields<'a>(
        question: &'a Option<String>,
        fields: &'a HashMap<String, String>,
    ) -> &'a dyn std::fmt::Debug {
        question
            .as_ref()
            .map_or(fields as &dyn std::fmt::Debug, |q| {
                q as &dyn std::fmt::Debug
            })
    }
}

impl PartialEq for Note {
    fn eq(&self, other: &Self) -> bool {
        let matching =
            self.deck == other.deck && self.model == other.model && self.tags == other.tags;

        let a_fields = self
            .fields
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, v)| (MatchUnescape::from(k), MatchUnescape::from(v)))
            .collect::<HashSet<_>>();
        let b_fields = other
            .fields
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, v)| (MatchUnescape::from(k), MatchUnescape::from(v)))
            .collect::<HashSet<_>>();
        let fields_match = a_fields == b_fields;

        if let (Some(s_id), Some(o_id)) = (self.id, other.id) {
            if s_id != o_id {
                error!("Id differs {} != {} but contents are the same (deck {}, model {}, fields {:?}, tags {:?})",
                    s_id, o_id, self.deck, self.model, self.fields, self.tags)
            }
        }

        matching && fields_match
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct MatchUnescape(String);

impl From<&String> for MatchUnescape {
    fn from(value: &String) -> Self {
        value.as_str().into()
    }
}

impl From<&str> for MatchUnescape {
    fn from(s: &str) -> Self {
        let s = s.replace(|x: char| x.is_whitespace(), "");
        let s = s.replace("&gt;", ">");
        let s = s.replace("&lt;", "<");
        Self(s)
    }
}


