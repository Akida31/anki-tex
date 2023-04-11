#![warn(clippy::perf)]
#![allow(clippy::uninlined_format_args, clippy::missing_const_for_fn, clippy::redundant_pub_crate)]

use anki_tex::*;
use clap::Parser;
use color_eyre::{
    eyre::{eyre, Context, Result},
    Help,
};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use std::{
    collections::{HashMap, HashSet},
    fs::read_to_string,
    path::{Path, PathBuf},
};
use tracing::{debug, error, info, warn, Level};
use tracing_subscriber::FmtSubscriber;

#[derive(Debug, PartialEq)]
struct Model {
    field_names: Vec<String>,
}

#[derive(Debug)]
struct State {
    deck_names: Vec<String>,
    models: HashMap<String, Model>,
    added_notes: Vec<Note>,
    last_hash: u64,
}

impl State {
    fn load_models() -> Result<HashMap<String, Model>> {
        let model_names = get_model_names()?.0;
        get_model_field_names_multi(model_names.iter().map(|n| n.as_str()))?
            .into_iter()
            .zip(model_names.into_iter())
            .map(|(field_names, name)| {
                Ok((
                    name,
                    Model {
                        field_names: field_names.0,
                    },
                ))
            })
            .collect()
    }

    fn new() -> Result<Self> {
        debug!("loading state");
        let models = Self::load_models()?;
        Ok(Self {
            deck_names: get_deck_names()?.0,
            models,
            added_notes: get_notes("*")?,
            last_hash: 0,
        })
    }

    // TODO reload state less often
    fn reload(&mut self) -> Result<()> {
        debug!("reloading state");
        self.deck_names = get_deck_names()?.0;
        self.models = Self::load_models()?;

        Ok(())
    }
}

fn get_notes(query: &str) -> Result<Vec<Note>> {
    let ids = find_notes(query)?;
    info!("getting {} notes", ids.len());
    let notes = notes_info(&ids)?;
    let card_ids = notes
        .iter()
        .flat_map(|note_info| note_info.cards.clone())
        .collect::<Vec<_>>();
    let cards = cards_info(&card_ids)?;
    assert_eq!(card_ids.len(), cards.len());
    let mut cards = cards.into_iter();

    notes
        .into_iter()
        .map(|note_info| {
            let fields = note_info
                .fields
                .into_iter()
                .map(|(name, field)| (name, field.value))
                .collect();

            let mut deck_name = None;
            let mut question = None;
            for _ in 0..note_info.cards.len() {
                let card = cards.next().unwrap();
                let n = card.deck_name;
                if let Some(name) = deck_name.as_ref() {
                    assert_eq!(&n, name);
                } else {
                    deck_name = Some(n);
                }
                question = Some(card.question);
            }

            Ok(Note {
                id: Some(note_info.note_id),
                deck: deck_name.unwrap(),
                model: note_info.model_name,
                fields,
                tags: note_info.tags,
                question,
            })
        })
        .collect()
}

const HEADER: &str = r#"\documentclass{article}

% formatting and layout
\usepackage[left=2.5cm, right=2.5cm, bottom=2.5cm]{geometry}
\usepackage[onehalfspacing]{setspace}
\setlength{\parindent}{0pt}

% input/output language
\usepackage[utf8]{inputenc}
\usepackage[T1]{fontenc}
\usepackage[ngerman]{babel}

% math packages
\usepackage{amsmath, amsfonts, amsthm}

\newcommand{\mysign}[2]{\phantom{|}\mathrel{\overset{\makebox[0pt]{\mbox{\tiny {#1}}}}{#2}}\phantom{|}}
\newcommand{\myeq}[1]{\mysign{#1}{=}}
\newcommand{\N}[0]{\mathbb{N}}
\newcommand{\Z}[0]{\mathbb{Z}}
\newcommand{\Q}[0]{\mathbb{Q}}
\newcommand{\R}[0]{\mathbb{R}}
\newcommand{\C}[0]{\mathbb{C}}
\newcommand{\e}[0]{\varepsilon}
\renewcommand{\Re}{\mathrm{Re}}
\renewcommand{\Im}{\mathrm{Im}}
\newcommand{\folge}[1]{\left(#1\right)_{n \in \N}}

\newcommand{\deck}[1]{\Large{Deck: #1}}
\newcommand{\model}[1]{\Large{Model: #1}}
\newcommand{\next}[0]{\vspace{2ex}\rule{\textwidth}{1pt}\par\vspace{2ex}\addpenalty{-1000}}
\renewcommand{\tag}[1]{\large{Tag #1}\par}
\newcommand{\fields}[2]{\large{\underline{#1:}}\\#2\\}
\newenvironment{field}[1]{\large{\underline{#1:}}\\}{\par}

\begin{document}
"#;
const FOOTER: &str = r"\end{document}";

#[derive(Debug, Clone, Copy, PartialEq)]
enum Cmd {
    Deck,
    Model,
    Field,
    Next,
    Tag,
}

mod re {
    use crate::Cmd;
    macro_rules! reg {
        ($name:ident = $cmd:path = $mat:literal) => {
            lazy_static::lazy_static! {
                pub(super) static ref $name: (Cmd, regex::Regex) = ($cmd, regex::Regex::new($mat).unwrap());
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

    pub(super) fn get_all_matches(text: &str) -> Vec<(usize, crate::Cmd, Option<regex::Captures>)> {
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
}

#[derive(Debug, Clone)]
struct Note {
    id: Option<usize>,
    deck: String,
    model: String,
    fields: HashMap<String, String>,
    tags: Vec<String>,
    // just for error messages
    question: Option<String>,
}

impl Note {
    fn question_or_fields<'a>(
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

fn escape(s: &str) -> String {
    let s = s.replace('>', "&gt;");

    s.replace('<', "&lt;")
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

fn get_header_and_footer(config: &Option<Config>) -> Result<(String, String)> {
    let (header_path, footer_path) = get_template_files(config);
    let header = if header_path.is_file() {
        read_to_string(&header_path).with_note(|| {
            eyre!(
                "while reading header template from {}",
                header_path.to_string_lossy()
            )
        })?
    } else {
        HEADER.to_owned()
    };
    let footer = if footer_path.is_file() {
        read_to_string(&footer_path).with_note(|| {
            eyre!(
                "while reading footer template from {}",
                header_path.to_string_lossy()
            )
        })?
    } else {
        FOOTER.to_owned()
    };

    Ok((header, footer))
}

fn create_template(config: &Option<Config>, path: &Path, force: bool) -> Result<()> {
    use std::io::Write;

    let (header, footer) = get_header_and_footer(config)?;

    if path.is_file() {
        if force {
            warn!("overwriting file {}", path.to_string_lossy());
        } else {
            return Err(eyre!(
                "file {} already exists. Use `--force` to overwrite",
                path.to_string_lossy()
            ));
        }
    }
    if path.is_dir() {
        return Err(eyre!(
            "Cannot create file {}. There is a folder with the same name",
            path.to_string_lossy()
        ));
    }
    let mut file = std::fs::File::create(path)?;
    file.write_all(header.as_bytes())?;
    file.write_all(b"\n% Add your content here\n\n")?;
    file.write_all(footer.as_bytes())?;

    Ok(())
}

fn get_longest_common_prefix(a: &str, b: &str) -> Option<usize> {
    for (i, (c, d)) in a.chars().zip(b.chars()).enumerate() {
        if c != d {
            return Some(i);
        }
    }
    None
}

fn get_content(content: String, header: &str) -> Result<Vec<Note>> {
    let content = content.trim();
    let content = match content.strip_prefix(header) {
        Some(content) => content,
        None => {
            return Err(eyre!("file does not start with required header")
                .with_note(|| {
                    format!(
                        "started instead with: {}",
                        &content[..content.len().min(50)]
                    )
                })
                .with_note(|| match get_longest_common_prefix(content, header) {
                    Some(i) => {
                        format!(
                            "they differ at char {}: required {} got {}",
                            i,
                            content.chars().nth(i).unwrap(),
                            header.chars().nth(i).unwrap(),
                        )
                    }
                    None => {
                        format!(
                            "file is too short, expected min {} characters but it has {}",
                            header.len(),
                            content.len(),
                        )
                    }
                }))
        }
    };
    let content = match content.strip_suffix(FOOTER) {
        Some(content) => content,
        None => {
            return Err(
                eyre!("file does not end with required header").with_note(|| {
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
    for (_start, cmd, cap) in re::get_all_matches(content) {
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

fn fmt_content(content: &String) -> String {
    format!(
        "[latex]{}[/latex]",
        content //.replace("\\]", "$$").replace("\\[", "$$")
    )
}

fn update_change(
    state: &mut State,
    config: &Option<Config>,
    file: &Path,
    add_generated: bool,
) -> Result<()> {
    if file.is_dir() {
        debug!(
            "{} is a directory. Updating children instead",
            file.to_string_lossy()
        );
        let children = std::fs::read_dir(file)
            .with_note(|| eyre!("while collecting children of {}", file.to_string_lossy()))?;
        for read_dir in children {
            let file = read_dir?.path();
            update_change(state, config, &file, add_generated)?;
        }

        return Ok(());
    }
    let content = read_to_string(file).context("while reading file")?;
    let new_hash = fasthash::metro::hash64(&content);
    if new_hash != state.last_hash {
        state.last_hash = new_hash;
    } else {
        debug!("nothing changed");
        return Ok(());
    }
    state.reload()?;

    let (header, _) = get_header_and_footer(config)?;

    let mut added_notes = 0;

    for mut note in get_content(content, &header)? {
        // TODO id
        if !state.deck_names.contains(&note.deck) {
            error!("create note with invalid deck name {}", note.deck);
            return Ok(());
        }
        let Some(model) = state.models.get(&note.model) else {
            error!("create note with invalid model name {}", note.model);
            return Ok(());
        };
        for field in note.fields.values_mut() {
            *field = fmt_content(field);
        }
        for field_name in note.fields.keys() {
            if !model.field_names.contains(field_name) {
                error!(
                    "model {} does not contain field `{}`",
                    note.model, field_name
                );
                info!("field names: {}", model.field_names.join(", "));
                return Ok(());
            }
        }

        if add_generated {
            note.tags.push(String::from("generated"));
        }

        if state.added_notes.contains(&note) {
            continue;
        }
        info!(
            "creating note in deck {} with fields {:?}",
            &note.deck,
            Note::question_or_fields(&note.question, &note.fields),
        );

        // TODO use `multi` api or `add_notes`
        let id = add_note(&anki_tex::Note {
            deck_name: note.deck.clone(),
            model_name: note.model.clone(),
            fields: note.fields.clone(),
            tags: note.tags.clone(),
        })?;
        if id.is_none() {
            info!("Duplicate! Note in deck {} already existed", &note.deck,);
        } else {
            added_notes += 1;
        }
        note.id = id;
        state.added_notes.push(note);
    }

    if added_notes == 0 {
        info!("nothing to do :)");
    } else {
        info!("added {} new notes", added_notes);
    }

    Ok(())
}

fn watch(config: &Option<Config>, file: PathBuf, add_generated: bool) -> Result<()> {
    let mut state = State::new()?;
    let file_c = file.clone();
    update_change(&mut state, config, &file, add_generated)?;

    let (tx, rx) = std::sync::mpsc::channel();

    let mut watcher = notify::recommended_watcher(tx)?;
    watcher.watch(&file_c, RecursiveMode::Recursive)?;

    info!("You can exit with Ctrl+C");
    for res in rx {
        let event: Event = res?;
        match event.kind {
            EventKind::Access(_) => {}
            EventKind::Create(_) => error!("file was created but should have existed before"),
            // TODO finer
            EventKind::Modify(_) => {
                if let Err(e) = update_change(&mut state, config, &file, add_generated) {
                    error!("{:#?}", e);
                }
            }
            EventKind::Any | EventKind::Other => {
                error!("unknown file watcher event: {:?}", event);
            }
            EventKind::Remove(_) => {
                watcher.watch(&file_c, RecursiveMode::Recursive)?;
                if !file.is_file() {
                    error!("file was removed.")
                } else if let Err(e) = update_change(&mut state, config, &file, add_generated) {
                    error!("{}", e);
                }
            }
        }
    }

    info!("Exiting");

    Ok(())
}

fn get_template_files(config: &Option<Config>) -> (PathBuf, PathBuf) {
    let (header_path, footer_path) = match config {
        Some(config) => (config.header_file.clone(), config.footer_file.clone()),
        None => (None, None),
    };
    let header_path = header_path.unwrap_or_else(|| "header_template.tex".into());
    let footer_path = footer_path.unwrap_or_else(|| "footer_template.tex".into());

    (header_path, footer_path)
}

/// Create Anki notes from file
#[derive(clap::Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path of the file to read from.
    ///
    /// If no value is given and no config file exists `anki.tex` will be used.
    #[arg(short, long)]
    path: Option<PathBuf>,

    /// Log Level
    #[arg(long, default_value = "info")]
    log_level: Level,
    /// Use short log output
    #[arg(long)]
    short_log: bool,
    /// Add a tag with the value `generated` for each new note.
    #[arg(long, default_value = "true")]
    add_generated: bool,

    #[command(subcommand)]
    subcommand: Commands,
}

#[derive(Debug, clap::Subcommand)]
enum Commands {
    /// Create the tex file from template
    Template {
        /// Whether to overwrite the file if it exists
        #[arg(short, long)]
        force: bool,
    },
    /// Save the template header and footer to config path
    SaveTemplate,
    /// Watch for changes and create new notes
    Watch,
    /// Create new notes
    #[clap(visible_alias = "c")]
    Create,
    /// Get all deck names
    GetDecks,
    /// Get all model names
    GetModels,
    /// Get all Notes for the given query
    GetNotes {
        /// See https://docs.ankiweb.net/searching.html
        #[arg(default_value = "*")]
        query: String,
    },
    /// Render all latex
    #[clap(visible_alias = "r")]
    Render,
    /// Sync all notes to ankiweb
    #[clap(visible_alias = "s")]
    Sync,
    /// Create, render and sync all notes to ankiweb
    Crs,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Config {
    path: Option<PathBuf>,
    header_file: Option<PathBuf>,
    footer_file: Option<PathBuf>,
}

fn main() -> Result<()> {
    color_eyre::install()?;

    let mut args = Args::parse();

    let builder = FmtSubscriber::builder().with_max_level(args.log_level);

    if args.short_log {
        let subscriber = builder.without_time().compact().finish();
        tracing::subscriber::set_global_default(subscriber)?;
    } else {
        tracing::subscriber::set_global_default(builder.finish())?;
    }

    let project_dirs = directories_next::ProjectDirs::from("", "akida", "anki-tex")
        .expect("no valid home directory path could be found");
    let config_dir = project_dirs.config_dir();
    if !config_dir.is_dir() {
        std::fs::create_dir_all(config_dir)?;
    }
    let config_path = config_dir.join("config.toml");

    let config = if !config_path.is_file() {
        info!(
            "no config file found. You can create one at {}",
            config_path.to_string_lossy()
        );
        None
    } else {
        let config_text = read_to_string(&config_path).with_note(|| {
            eyre!(
                "while reading config file from {}",
                config_path.to_string_lossy()
            )
        })?;
        let config: Config = toml::from_str(&config_text).with_note(|| {
            eyre!(
                "while parsing config file from {}",
                config_path.to_string_lossy()
            )
        })?;

        // update the args
        args.path = args.path.or_else(|| config.path.clone());

        Some(config)
    };

    let path = args.path.unwrap_or_else(|| "anki.tex".into());

    match args.subcommand {
        Commands::Template { force } => create_template(&config, &path, force)?,
        Commands::Watch => watch(&config, path, args.add_generated)?,
        Commands::Create => {
            let mut state = State::new()?;
            update_change(&mut state, &config, &path, args.add_generated)?;
        }
        Commands::GetDecks => {
            let names = get_deck_names()?;
            println!("All deck names: \n {}", names.0.join("\n "))
        }
        Commands::GetModels => {
            let names = get_model_names()?;
            println!("All model names: \n {}", names.0.join("\n "))
        }
        Commands::GetNotes { query } => {
            let notes = get_notes(&query)?;
            let notes_len = notes.len();

            for note in notes {
                println!("In deck {} with model {}", note.deck, note.model);
                for (k, v) in note.fields {
                    let v = v.replace("[latex]", "");
                    let v = v.replace("[/latex]", "");
                    println!("[{}] {}", k, v);
                }
                if !note.tags.is_empty() {
                    println!("Tags: {}", note.tags.join(", "));
                }
                println!("{}", "-".repeat(80));
            }

            println!("fetched {} notes in total", notes_len);
        }
        Commands::Render => {
            info!("rendering all latex");
            if render_all_latex()? {
                println!("Success");
            } else {
                println!("Error :(");
            }
        }
        Commands::Sync => {
            info!("syncing all notes");
            render_all_latex()?;
            println!("Success");
        }
        Commands::Crs => {
            // TODO remove duplication
            let mut state = State::new()?;
            update_change(&mut state, &config, &path, args.add_generated)?;
            info!("rendering all latex");
            if render_all_latex()? {
                println!("Success");
            } else {
                println!("Error :(");
            }
            info!("syncing all notes");
            render_all_latex()?;
            println!("Success");
        }
        Commands::SaveTemplate => {
            let (header_path, footer_path) = get_template_files(&config);
            std::fs::write(&header_path, HEADER).with_note(|| {
                eyre!(
                    "while writing header template to {}",
                    header_path.to_string_lossy()
                )
            })?;
            info!("wrote header template to {}", header_path.to_string_lossy());

            std::fs::write(&footer_path, FOOTER).with_note(|| {
                eyre!(
                    "while writing footer template to {}",
                    header_path.to_string_lossy()
                )
            })?;
            info!("wrote footer template to {}", footer_path.to_string_lossy());
        }
    }

    Ok(())
}
