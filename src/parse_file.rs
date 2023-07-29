use std::collections::HashMap;
use color_eyre::{Help, eyre::{eyre, Result}};
use tracing::warn;
use crate::Note;

pub const ANKITEX: &str = include_str!("ankitex.sty");
pub const CUSTOM_TEMPLATE: &str = include_str!("custom.sty");

pub const HEADER: &str = r"\documentclass{article}
\usepackage{ankitex}
\usepackage{custom}

\begin{document}
";
pub const FOOTER: &str = r"\end{document}";

#[derive(Debug, Clone, Copy, PartialEq)]
enum Cmd {
    Deck,
    Model,
    Field,
    Next,
    Tag,
}

macro_rules! reg {
        ($name:ident = $cmd:path = $mat:literal) => {
            lazy_static::lazy_static! {
                static ref $name: (Cmd, regex::Regex) = ($cmd, regex::Regex::new($mat).unwrap());
            }
        };
        ($($name:ident = $cmd:path = $mat:literal),*$(,)?) => {
            $(reg!($name = $cmd = $mat);)*
        }
    }

reg![
    DECK = Cmd::Deck = r"\\deck\{([^\}]*)\}",
    MODEL = Cmd::Model = r"\\model\{([^\}]*)\}",
    TAG = Cmd::Tag = r"\\tag\{([^\}]*)\}",
    NEXT = Cmd::Next = r"\\next",
    FIELD = Cmd::Field = r"\\fields\{([^\}]*)\}\{([^\}]*)\}",
    FIELD_ENV = Cmd::Field = r"\\begin\{field\}\{([^\}]*)\}([\s\S]*?)\\end\{field\}",
];

fn get_all_matches(text: &str) -> Vec<(usize, Cmd, Option<regex::Captures>)> {
    let mut locations = Vec::new();

    for mat in NEXT.1.find_iter(text) {
        locations.push((mat.start(), NEXT.0, None));
    }

    for (cmd, re) in &[&*DECK, &MODEL, &TAG, &FIELD, &FIELD_ENV] {
        for mat in re.find_iter(text) {
            let start = mat.start();
            let group = re.captures(&text[start..]).unwrap();
            locations.push((start, *cmd, Some(group)));
        }
    }

    locations.sort_by_cached_key(|(start, _, _)| *start);

    locations
}

pub fn get_content(content: String) -> Result<Vec<Note>> {
    let content = content.trim();
    let content = match content.strip_prefix(HEADER) {
        Some(content) => content,
        None => {
            let longest_prefix = get_longest_common_prefix(content, HEADER);
            let longest_prefix_note = match longest_prefix {
                Some(i) => {
                    format!(
                        "they differ at char {}: required `{}` got `{}`",
                        i,
                        content.chars().nth(i).unwrap(),
                        HEADER.chars().nth(i).unwrap(),
                    )
                }
                None => {
                    format!(
                        "file is too short, expected min {} characters but it has {}",
                        HEADER.len(),
                        content.len(),
                    )
                }
            };
            let (required_line, got_line) = match longest_prefix {
                Some(i) => (
                    format!("required line `{}`", get_line_with_pos(HEADER, i)),
                    format!("got line `{}`", get_line_with_pos(content, i)),
                ),
                None => Default::default(),
            };
            return Err(eyre!("file does not start with required header")
                .with_note(|| {
                    format!(
                        "started instead with: {}",
                        &content[..content.len().min(50)]
                    )
                })
                .note(longest_prefix_note)
                .note(required_line)
                .note(got_line));
        }
    };
    let content = match content.strip_suffix(FOOTER) {
        Some(content) => content,
        None => {
            return Err(
                eyre!("file does not end with required footer").with_note(|| {
                    format!(
                        "ended instead with: {}",
                        &content[content.len().max(50) - 50..]
                    )
                }),
            )
        }
    };

    let mut current_deck = None;
    let mut current_model = None;
    let mut current_tags = Vec::new();
    let mut current_fields = HashMap::new();
    let mut completed_notes = Vec::new();

    // TODO use _start
    for (_start, cmd, cap) in get_all_matches(content) {
        match cmd {
            Cmd::Deck => {
                // TODO remove last unwrap
                let new = cap.unwrap().get(1).unwrap().as_str();
                current_deck = Some(new.to_owned());
            }
            Cmd::Model => {
                // TODO remove last unwrap
                let new = cap.unwrap().get(1).unwrap().as_str();
                current_model = Some(new.to_owned());
            }
            Cmd::Tag => {
                // TODO remove last unwrap
                let new = cap.unwrap().get(1).unwrap().as_str().to_owned();
                if current_tags.contains(&new) {
                    return Err(eyre!("Can't add tag {} multiple times", new));
                }
                current_tags.push(new);
            }
            Cmd::Field => {
                let cap = cap.unwrap();
                // TODO remove last unwrap
                let name = cap.get(1).unwrap().as_str().to_owned();
                let content = cap.get(2).unwrap().as_str().to_owned();
                if current_fields.contains_key(&name) {
                    return Err(eyre!("Field `{}` was already added", name));
                }
                current_fields.insert(name, content);
            }
            Cmd::Next => {
                let Some(deck) = current_deck.clone() else {
                    return Err(eyre!("Select a deck before ending a note"));
                };
                let Some(model) = current_model.clone() else {
                    return Err(eyre!("Select a model before ending a note"));
                };
                if current_fields.is_empty() {
                    return Err(eyre!("Cannot add note without fields"));
                }
                let tags = std::mem::take(&mut current_tags);
                let fields = std::mem::take(&mut current_fields);
                completed_notes.push(Note {
                    id: None,
                    deck,
                    model,
                    fields,
                    tags,
                    question: None,
                });
            }
        }
    }

    if !current_fields.is_empty() || !current_tags.is_empty() {
        warn!(
            "dismissing unfinished note with fields {:?}",
            current_fields
        );
    }

    if completed_notes.is_empty() {
        warn!("no completed notes found");
    }

    Ok(completed_notes)
}

fn get_longest_common_prefix(a: &str, b: &str) -> Option<usize> {
    for (i, (c, d)) in a.chars().zip(b.chars()).enumerate() {
        if c != d {
            return Some(i);
        }
    }
    None
}

fn get_line_with_pos(text: &str, pos: usize) -> &str {
    let mut offset = 0;
    for line in text.lines() {
        offset += line.len();
        if offset >= pos {
            return line;
        }
    }
    ""
}

