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
    last_main_hash: u64,
    last_custom_hash: u64,
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
            last_main_hash: 0,
            last_custom_hash: 0,
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

struct FilePaths {
    main: PathBuf,
    anki: PathBuf,
    custom: PathBuf,
}

impl FilePaths {
    fn from_main(main: PathBuf) -> Result<Self> {
        let parent = main
            .parent()
            .ok_or_else(|| eyre!("{} has no parent", main.to_string_lossy()))?;
        let anki = parent.join("ankitex.sty");
        let custom = parent.join("custom.sty");

        Ok(Self { main, anki, custom })
    }
}

fn create_template(config: &Config, paths: &FilePaths, force: bool) -> Result<()> {
    use std::io::Write;

    let main = [
        parse_file::HEADER,
        "\n% Add your content here\n\n",
        parse_file::FOOTER,
    ];
    let anki = [parse_file::ANKITEX];
    let custom = [parse_file::CUSTOM_TEMPLATE];

    let files: &[(&Path, &[&str])] = &[
        (&paths.main, &main),
        (&paths.anki, &anki),
        (&paths.custom, &custom),
    ];
    for (filepath, content) in files {
        if config.is_ignored(&filepath.to_string_lossy()) {
            return Err(eyre!("template file is excluded"));
        }

        if filepath.is_file() {
            if force {
                warn!("overwriting file {}", filepath.to_string_lossy());
            } else {
                return Err(eyre!(
                    "file {} already exists. Use `--force` to overwrite",
                    filepath.to_string_lossy()
                ));
            }
        }
        if filepath.is_dir() {
            return Err(eyre!(
                "Cannot create file {}. There is a folder with the same name",
                filepath.to_string_lossy()
            ));
        }
        let mut file = std::fs::File::create(filepath)?;
        for c in *content {
            file.write_all(c.as_bytes())?;
        }
    }

    {
        debug!("marking `ankitex.sty` as readonly");
        if let Ok(m) = std::fs::metadata(&paths.anki) {
            let mut perms = m.permissions();
            perms.set_readonly(true);
            if let Err(e) = std::fs::set_permissions(&paths.anki, perms) {
                info!(
                    "failed to mark {} as readonly: {}",
                    paths.anki.to_string_lossy(),
                    e
                );
            }
        }
    }

    Ok(())
}

fn fmt_content(content: &String) -> String {
    format!(
        "[latex]{}[/latex]",
        content //.replace("\\]", "$$").replace("\\[", "$$")
    )
}

fn update_change(state: &mut State, config: &Config, paths: &FilePaths) -> Result<()> {
    if config.is_ignored(&paths.main.to_string_lossy()) {
        return Ok(());
    }
    if paths.main.is_dir() {
        debug!(
            "{} is a directory. Updating children instead",
            paths.main.to_string_lossy()
        );
        let children = std::fs::read_dir(&paths.main).with_note(|| {
            eyre!(
                "while collecting children of {}",
                paths.main.to_string_lossy()
            )
        })?;
        for read_dir in children {
            // TODO is this correct or should the anki and custom path be changed as well?
            let new_paths = FilePaths {
                main: read_dir?.path(),
                anki: paths.anki.clone(),
                custom: paths.custom.clone(),
            };
            update_change(state, config, &new_paths)?;
        }

        return Ok(());
    }
    let main_content = read_to_string(&paths.main)
        .with_note(|| eyre!("while reading file {}", paths.main.to_string_lossy()))?;


    parse_file::check_ankitex_template(&paths.anki)?;

    // TODO do something with paths.custom. E.g. check that it is correctly set as template

    let new_main_hash = fasthash::metro::hash64(&main_content);
    let new_custom_hash = fasthash::metro::hash64(&main_content);
    if new_main_hash != state.last_main_hash {
        state.last_main_hash = new_main_hash;
    } else if new_custom_hash != state.last_custom_hash {
        state.last_custom_hash = new_custom_hash;
    } else {
        debug!("nothing changed");
        return Ok(());
    }
    info!("updating changes from {}", paths.main.to_string_lossy());
    state.reload()?;

    let mut added_notes = 0;

    for mut note in parse_file::get_content(main_content)? {
        // TODO id
        if !state.deck_names.contains(&note.deck) {
            error!("create note with invalid deck name {}", note.deck);
            info!("create all decks in the file with `anki-tex create-all-decks`");
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

fn watch(config: &Config, paths: &FilePaths) -> Result<()> {
    let mut state = State::new()?;
    update_change(&mut state, config, paths)?;

    let (tx, rx) = std::sync::mpsc::channel();

    let mut watcher = notify::recommended_watcher(tx)?;
    watcher.watch(&paths.main, RecursiveMode::Recursive)?;
    watcher.watch(&paths.custom, RecursiveMode::NonRecursive)?;

    info!("You can exit with Ctrl+C");
    for res in rx {
        let event: Event = res?;
        match event.kind {
            EventKind::Access(_) => {}
            EventKind::Create(_) => error!("file was created but should have existed before"),
            // TODO finer
            EventKind::Modify(_) => {
                if let Err(e) = update_change(&mut state, config, paths) {
                    error!("{:#?}", e);
                }
            }
            EventKind::Any | EventKind::Other => {
                error!("unknown file watcher event: {:?}", event);
            }
            EventKind::Remove(_) => {
                // TODO is this necessary?
                watcher.watch(&paths.main, RecursiveMode::Recursive)?;
                watcher.watch(&paths.custom, RecursiveMode::NonRecursive)?;
                if !paths.main.is_file() {
                    error!("file was removed.")
                } else if let Err(e) = update_change(&mut state, config, paths) {
                    error!("{}", e);
                }
            }
        }
    }

    info!("Exiting");

    Ok(())
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
    /// Save the template files (`anki.tex`, `ankitex.sty` and `custom.sty`) to the project directory.
    Template {
        /// Whether to overwrite the file if it exists
        #[arg(short, long)]
        force: bool,
    },
    /// Watch for changes and create new notes
    Watch,
    /// Create new notes
    #[clap(visible_alias = "c")]
    Create,
    /// Create all decks in the file if they don't exist already
    CreateAllDecks,
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

struct Config {
    path: Option<PathBuf>,
    file_include: Vec<RegexString>,
    file_exclude: Vec<RegexString>,
    add_generated: bool,
    add_generation_date: Option<String>,
}

impl Config {
    fn load(add_generated: bool, add_generation_date: Option<String>) -> Result<Self> {
        #[derive(Default, serde::Deserialize)]
        struct ExternalConfig {
            path: Option<PathBuf>,
            #[serde(default)]
            file_include: Vec<RegexString>,
            #[serde(default)]
            file_exclude: Vec<RegexString>,
        }

        let project_dirs = directories_next::ProjectDirs::from("", "akida", "anki-tex")
            .expect("no valid home directory path could be found");
        let config_dir = project_dirs.config_dir();
        if !config_dir.is_dir() {
            std::fs::create_dir_all(config_dir)?;
        }
        let config_path = config_dir.join("config.toml");

        let config: ExternalConfig = if !config_path.is_file() {
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

        Ok(Self {
            path: config.path,
            file_include: config.file_include,
            file_exclude: config.file_exclude,
            add_generated,
            add_generation_date,
        })
    }

    fn is_ignored(&self, path: &str) -> bool {
        if !self.file_include.iter().any(|r| r.re.is_match(path)) {
            info!(
                "ignoring {} because it is not included (regex={})",
                path,
                self.file_include
                    .iter()
                    .map(|r| format!("\"{}\"", r.re_str))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            return true;
        }
        for RegexString { re, re_str } in &self.file_exclude {
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

fn create_all_decks(paths: &FilePaths) -> Result<()> {
    let main_content = read_to_string(&paths.main)
        .with_note(|| eyre!("while reading file {}", paths.main.to_string_lossy()))?;

    debug!("parsing file for used decks");
    let used_decks = parse_file::get_used_decks(main_content)?;

    let used_decks = used_decks
        .into_iter()
        .flat_map(|full| {
            let mut decks = Vec::new();
            let mut prefix = String::new();

            for part in full.split("::") {
                if !prefix.is_empty() {
                    prefix.push_str("::");
                }
                prefix.push_str(part);
                decks.push(prefix.clone());
            }

            decks
        })
        .collect::<Vec<_>>();

    debug!("collecting available decks from anki");
    let available_decks: HashSet<_> = get_deck_names()?.0.into_iter().collect();

    let mut created: HashSet<String> = HashSet::new();

    for deck in used_decks {
        if available_decks.contains(&deck) || created.contains(&deck) {
            continue;
        }
        if api::create_deck(&deck)?.is_some() {
            info!("created deck {}", deck);
        }
        assert!(created.insert(deck));
    }

    if created.is_empty() {
        info!("All decks were already created")
    }

    Ok(())
}

fn main() -> Result<()> {
    color_eyre::install()?;

    let args = Args::parse();

    let builder = FmtSubscriber::builder().with_max_level(args.log_level);

    if args.short_log {
        let subscriber = builder.without_time().compact().finish();
        tracing::subscriber::set_global_default(subscriber)?;
    } else {
        tracing::subscriber::set_global_default(builder.finish())?;
    }

    let config = Config::load(
        args.add_generated,
        args.add_generation_date
            .then(|| format!("{}", chrono::Local::now().format("%Y-%m-%d"))),
    )?;

    let child = args.path.unwrap_or_else(|| "anki.tex".into());
    let main_path = if let Some(parent) = &config.path {
        if child.is_relative() {
            parent.join(child)
        } else {
            child
        }
    } else {
        child
    };

    let paths = FilePaths::from_main(main_path)?;

    // drop args so it can't be used later on
    let Args { subcommand, .. } = args;

    match subcommand {
        Commands::Template { force } => create_template(&config, &paths, force)?,
        Commands::Watch => watch(&config, &paths)?,
        Commands::Create => {
            let mut state = State::new()?;
            update_change(&mut state, &config, &paths)?;
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
        Commands::CreateAllDecks => {
            create_all_decks(&paths)?;
        }
        Commands::Sync => {
            info!("syncing all notes");
            sync()?;
            println!("Success");
        }
        Commands::Crs => {
            // TODO remove duplication
            let mut state = State::new()?;
            update_change(&mut state, &config, &paths)?;
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
    }

    Ok(())
}
