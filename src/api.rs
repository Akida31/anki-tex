use std::{borrow::Cow, collections::HashMap};

use color_eyre::{Help, Result};
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::types::{self, empty, Request};

pub fn request<'a, T: Serialize + 'a, U: for<'de> Deserialize<'de> + std::fmt::Debug>(
    action: impl Into<Cow<'a, str>>,
    data: &'a T,
) -> Result<U> {
    let action = action.into();

    debug!("requesting action {}", action);
    let request = Request::new(action, data);
    let client = reqwest::blocking::Client::new();
    let res = client.post("http://localhost:8765").json(&request).send()?;

    debug!("got response with status {}", res.status());
    let bytes = res.bytes()?;
    let res: std::result::Result<types::ReqResult<U>, _> = serde_json::from_slice(&bytes);
    match res {
        Ok(v) => v.get(),
        Err(e) => Result::Err(e).with_note(|| format!("body: {}", String::from_utf8_lossy(&bytes))),
    }
}

pub fn request_multi<'a, T: Serialize + 'a, U: for<'de> Deserialize<'de> + std::fmt::Debug>(
    action: &str,
    data: impl IntoIterator<Item = T>,
) -> Result<Vec<U>> {
    #[derive(Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Params<'a, T: 'a> {
        actions: Vec<InnerParams<'a, T>>,
    }

    #[derive(Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct InnerParams<'a, T: 'a> {
        action: &'a str,
        params: T,
    }

    let res = request::<_, Vec<types::ReqResult<U>>>(
        "multi",
        &Params {
            actions: data
                .into_iter()
                .map(|params| InnerParams { action, params })
                .collect::<Vec<_>>(),
        },
    )?;
    res.into_iter().map(|r| r.get()).collect()
}

/// Returns
/// - `id` if the note was created
/// - `None` if the note wasn't created (e.g. duplicate)
pub fn create_deck(deck: &str) -> Result<Option<usize>> {
    #[derive(Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Params<'a> {
        deck: &'a str,
    }

    request("createDeck", &Params { deck })
}

#[derive(Debug, Deserialize)]
pub struct DeckNames(pub Vec<String>);

pub fn get_deck_names() -> Result<DeckNames> {
    request("deckNames", &empty())
}

#[derive(Debug, Deserialize)]
pub struct ModelNames(pub Vec<String>);

pub fn get_model_names() -> Result<ModelNames> {
    request("modelNames", &empty())
}

#[derive(Default, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    pub deck_name: String,
    pub model_name: String,
    pub fields: HashMap<String, String>,
    pub tags: Vec<String>,
    // TODO
    // options
    // audio
    // video
    // picture
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct NoteInfoField {
    pub value: String,
    pub order: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteInfo {
    pub note_id: usize,
    pub model_name: String,
    pub fields: HashMap<String, NoteInfoField>,
    pub tags: Vec<String>,
    pub cards: Vec<usize>,
}

pub fn add_notes(notes: &[Note]) -> Result<Vec<Option<usize>>> {
    #[derive(Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct NoteParams<'a> {
        notes: &'a [Note],
    }

    request("addNotes", &NoteParams { notes })
}

/// Returns
/// - `id` if the note was created
/// - `None` if the note wasn't created (e.g. duplicate)
pub fn add_note(note: &Note) -> Result<Option<usize>> {
    #[derive(Debug, Deserialize)]
    pub struct AddNote(Option<usize>);

    #[derive(Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct NoteParams<'a> {
        note: &'a Note,
    }

    let res = request::<_, AddNote>("addNote", &NoteParams { note });
    match res {
        Err(e)
            if e.root_cause()
                .to_string()
                .ends_with("cannot create note because it is a duplicate") =>
        {
            Ok(None)
        }
        Err(e) => Err(e),
        Ok(AddNote(res)) => Ok(res),
    }
}

#[derive(Debug, Deserialize)]
pub struct ModelFieldNames(pub Vec<String>);
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelFieldNameParams<'a> {
    model_name: &'a str,
}

pub fn get_model_field_names(model_name: &str) -> Result<ModelFieldNames> {
    request("modelFieldNames", &ModelFieldNameParams { model_name })
}

pub fn get_model_field_names_multi<'a>(
    model_names: impl IntoIterator<Item = impl Into<&'a str>>,
) -> Result<Vec<ModelFieldNames>> {
    request_multi(
        "modelFieldNames",
        model_names
            .into_iter()
            .map(|model_name| ModelFieldNameParams {
                model_name: model_name.into(),
            }),
    )
}

/// See https://docs.ankiweb.net/searching.html
pub fn find_notes(query: &str) -> Result<Vec<usize>> {
    #[derive(Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Params<'a> {
        query: &'a str,
    }

    request("findNotes", &Params { query })
}

pub fn notes_info(ids: &[usize]) -> Result<Vec<NoteInfo>> {
    #[derive(Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Params<'a> {
        notes: &'a [usize],
    }

    request("notesInfo", &Params { notes: ids })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CardInfo {
    pub answer: String,
    pub question: String,
    pub deck_name: String,
    pub model_name: String,
    pub field_order: i32,
    pub fields: HashMap<String, NoteInfoField>,
    pub css: String,
    pub card_id: usize,
    pub interval: i32,
    pub note: usize,
    pub ord: i32,
    pub r#type: i32,
    pub queue: i32,
    pub due: i32,
    pub reps: i32,
    pub lapses: i32,
    pub left: i32,
    pub r#mod: i32,
}

pub fn cards_info(ids: &[usize]) -> Result<Vec<CardInfo>> {
    #[derive(Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Params<'a> {
        cards: &'a [usize],
    }

    request("cardsInfo", &Params { cards: ids })
}

pub fn render_all_latex() -> Result<bool> {
    let res = request("renderAllLatex", &empty());
    match res {
        Err(e) => {
            let cause = e.root_cause().to_string();
            // TODO: don't hardcode this
            let Some(suffix) =
                cause.strip_prefix("anki returned an error: Can't render note with id ")
            else {
                debug!("invalid prefix");
                return Err(e);
            };
            let n = if let Some((n, _)) = suffix.split_once(':') {
                match n.parse() {
                    Ok(v) => v,
                    Err(_) => return Err(e),
                }
            } else {
                debug!("invalid format");
                return Err(e);
            };
            let Ok(info) = notes_info(&[n]) else {
                debug!("can't request note info");
                return Err(e);
            };
            Err(e.with_note(|| {
                let mut fields: Vec<_> = info[0].fields.iter().collect();
                fields.sort_by_key(|f| f.1.order);

                let fields = fields
                    .iter()
                    .map(|(k, v)| format!("{}: {}", k, v.value))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("fields of note: {}", fields)
            }))
        }
        v @ Ok(_) => v,
    }
}

pub fn sync() -> Result<()> {
    request("sync", &empty())
}
