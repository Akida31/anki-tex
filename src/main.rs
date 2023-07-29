#![warn(clippy::perf)]
#![allow(
    clippy::uninlined_format_args,
    clippy::missing_const_for_fn,
    clippy::redundant_pub_crate
)]

use anki_tex::*;
use clap::Parser;
use color_eyre::{
    eyre::{eyre, Result},
    Help,
};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use regex::Regex;
use serde::Deserialize;
use std::{
    collections::HashMap,
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
            .zip(model_names)
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

fn get_header_and_footer(config: &Config) -> Result<(String, String)> {
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

fn create_template(config: &Config, path: &Path, force: bool) -> Result<()> {
    use std::io::Write;

    let (header, footer) = get_header_and_footer(config)?;

    if config.is_ignored(&path.to_string_lossy()) {
        return Err(eyre!("template file is excluded"));
    }

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

fn fmt_content(content: &String) -> String {
    format!(
        "[latex]{}[/latex]",
        content //.replace("\\]", "$$").replace("\\[", "$$")
    )
}

fn update_change(state: &mut State, config: &Config, file: &Path) -> Result<()> {
    if config.is_ignored(&file.to_string_lossy()) {
        return Ok(());
    }
    if file.is_dir() {
        debug!(
            "{} is a directory. Updating children instead",
            file.to_string_lossy()
        );
        let children = std::fs::read_dir(file)
            .with_note(|| eyre!("while collecting children of {}", file.to_string_lossy()))?;
        for read_dir in children {
            let file = read_dir?.path();
            update_change(state, config, &file)?;
        }

        return Ok(());
    }
    let content = read_to_string(file)
        .with_note(|| eyre!("while reading file {}", file.to_string_lossy()))?;
    let new_hash = fasthash::metro::hash64(&content);
    if new_hash != state.last_hash {
        state.last_hash = new_hash;
    } else {
        debug!("nothing changed");
        return Ok(());
    }
    info!("updating changes from {}", file.to_string_lossy());
    state.reload()?;

    let (header, _) = get_header_and_footer(config)?;

    let mut added_notes = 0;

    for mut note in parse_file::get_content(content, &header)? {
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

        if config.add_generated {
            note.tags.push(String::from("generated"));
        }

        if let Some(date) = &config.add_generation_date {
            note.tags.push(date.clone());
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
        let id = add_note(&anki_tex::api::Note {
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

fn watch(config: &Config, file: PathBuf) -> Result<()> {
    let mut state = State::new()?;
    let file_c = file.clone();
    update_change(&mut state, config, &file)?;

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
                if let Err(e) = update_change(&mut state, config, &file) {
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
                } else if let Err(e) = update_change(&mut state, config, &file) {
                    error!("{}", e);
                }
            }
        }
    }

    info!("Exiting");

    Ok(())
}

fn get_template_files(config: &Config) -> (PathBuf, PathBuf) {
    let mut header_path = config
        .config
        .header_file
        .clone()
        .unwrap_or_else(|| "header_template.tex".into());
    let mut footer_path = config
        .config
        .footer_file
        .clone()
        .unwrap_or_else(|| "footer_template.tex".into());

    if header_path.is_relative() {
        header_path = config.config_dir.join(header_path);
    }
    if footer_path.is_relative() {
        footer_path = config.config_dir.join(footer_path);
    }

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
    /// Add a tag with the value `generated@$date` for each new note.
    #[arg(long, default_value = "true")]
    add_generation_date: bool,

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

#[derive(Debug)]
struct RegexString {
    re: Regex,
    re_str: String,
}

impl<'de> Deserialize<'de> for RegexString {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;
        let re_str: String = serde::Deserialize::deserialize(deserializer)?;

        let re = Regex::new(&re_str).map_err(D::Error::custom)?;

        Ok(Self { re, re_str })
    }
}

#[derive(Default, serde::Deserialize)]
struct ExternalConfig {
    path: Option<PathBuf>,
    header_file: Option<PathBuf>,
    footer_file: Option<PathBuf>,
    #[serde(default)]
    file_include: Vec<RegexString>,
    #[serde(default)]
    file_exclude: Vec<RegexString>,
}

impl ExternalConfig {
    fn load() -> Result<(Self, PathBuf)> {
        let project_dirs = directories_next::ProjectDirs::from("", "akida", "anki-tex")
            .expect("no valid home directory path could be found");
        let config_dir = project_dirs.config_dir();
        if !config_dir.is_dir() {
            std::fs::create_dir_all(config_dir)?;
        }
        let config_path = config_dir.join("config.toml");

        let config: Self = if !config_path.is_file() {
            info!(
                "no config file found. You can create one at {}",
                config_path.to_string_lossy()
            );
            Default::default()
        } else {
            let config_text = read_to_string(&config_path).with_note(|| {
                eyre!(
                    "while reading config file from {}",
                    config_path.to_string_lossy()
                )
            })?;
            toml::from_str(&config_text).with_note(|| {
                eyre!(
                    "while parsing config file from {}",
                    config_path.to_string_lossy()
                )
            })?
        };

        Ok((config, config_dir.to_path_buf()))
    }
}

struct Config {
    config: ExternalConfig,
    config_dir: PathBuf,
    add_generated: bool,
    add_generation_date: Option<String>,
}

impl Config {
    fn is_ignored(&self, path: &str) -> bool {
        for RegexString { re, re_str } in &self.config.file_include {
            if !re.is_match(path) {
                info!(
                    "ignoring {} because it is not included (regex={})",
                    path, re_str
                );
                return true;
            }
        }
        for RegexString { re, re_str } in &self.config.file_exclude {
            if re.is_match(path) {
                info!(
                    "ignoring {} because it is excluded (regex={})",
                    path, re_str
                );
                return true;
            }
        }
        false
    }
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

    let (config, config_dir) = ExternalConfig::load()?;

    let config = Config {
        config,
        config_dir,
        add_generated: args.add_generated,
        add_generation_date: args
            .add_generation_date
            .then(|| format!("{}", chrono::Local::now().format("%Y-%m-%d"))),
    };

    // update the args
    args.path = args.path.or_else(|| config.config.path.clone());

    let path = args.path.unwrap_or_else(|| "anki.tex".into());

    match args.subcommand {
        Commands::Template { force } => create_template(&config, &path, force)?,
        Commands::Watch => watch(&config, path)?,
        Commands::Create => {
            let mut state = State::new()?;
            update_change(&mut state, &config, &path)?;
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
            sync()?;
            println!("Success");
        }
        Commands::Crs => {
            // TODO remove duplication
            let mut state = State::new()?;
            update_change(&mut state, &config, &path)?;
            info!("rendering all latex");
            if render_all_latex()? {
                println!("Success");
            } else {
                println!("Error :(");
            }
            info!("syncing all notes");
            sync()?;
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
