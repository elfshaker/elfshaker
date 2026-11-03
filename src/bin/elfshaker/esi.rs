//! Commands for creating and editing elfshaker remote indexes.

use clap::{parser::ValueSource, Arg, ArgAction, ArgMatches, Command};
use elfshaker::esi::{
    commit_documents, digest_file, validate_document_writes, Document, DocumentWrite, Entry,
    MetaVersion,
};
use std::{
    error::Error,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    str::FromStr,
};

pub(crate) const SUBCOMMAND: &str = "esi";

pub(crate) fn run(matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    if matches.value_source("data_dir") == Some(ValueSource::CommandLine) {
        return Err("--data-dir does not apply to ESI file operations".into());
    }

    match matches.subcommand() {
        Some(("create", matches)) => create(matches),
        Some(("list", matches)) => list(matches),
        Some(("add", matches)) => add(matches),
        Some(("remove", matches)) => remove(matches),
        _ => Ok(()),
    }
}

fn create(matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let path = path_arg(matches, "esi_file");
    let force = matches.get_flag("force");
    if path.exists() && !force {
        return Err(format!(
            "'{}' already exists; use --force to replace it",
            path.display()
        )
        .into());
    }

    let meta = MetaVersion::from_str(matches.get_one::<String>("meta").unwrap())?;
    let url = matches.get_one::<String>("url").unwrap().to_owned();
    let document = Document::create(meta, url)?;
    commit_documents(vec![DocumentWrite {
        path,
        contents: document.to_bytes(),
        backup: !matches.get_flag("no_backup"),
    }])?;
    Ok(())
}

fn list(matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let (_, document) = load_document(&path_arg(matches, "esi_file"))?;
    for entry in document.entries() {
        print!(
            "{}\t{}\t{}",
            hex::encode(entry.index_checksum),
            hex::encode(entry.pack_checksum),
            entry.url
        );
        if document.meta() == MetaVersion::V2 {
            print!(
                "\t{}\t{}",
                entry.pack_size.expect("v2 entry without pack size"),
                entry.index_size.expect("v2 entry without pack index size")
            );
        }
        println!();
    }
    Ok(())
}

fn add(matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let paths = target_paths(matches)?;
    let mut documents = load_documents(&paths)?;
    let packs: Vec<PathBuf> = matches
        .get_many::<String>("pack")
        .unwrap()
        .map(PathBuf::from)
        .collect();
    let pack_url = matches.get_one::<String>("pack_url");
    if pack_url.is_some() && packs.len() != 1 {
        return Err("--pack-url requires exactly one <PACK>".into());
    }

    let mut entries = Vec::with_capacity(packs.len());
    for pack in &packs {
        let index_path = adjacent_index_path(pack);
        let pack_digest = digest_file(pack)
            .map_err(|error| format!("failed to read pack '{}': {error}", pack.display()))?;
        let index_digest = digest_file(&index_path).map_err(|error| {
            format!(
                "failed to read adjacent pack index '{}': {error}",
                index_path.display()
            )
        })?;
        let url = match pack_url {
            Some(url) => url.to_owned(),
            None => default_pack_url(pack)?,
        };
        entries.push(Entry {
            index_checksum: index_digest.checksum,
            pack_checksum: pack_digest.checksum,
            url,
            pack_size: Some(pack_digest.size),
            index_size: Some(index_digest.size),
        });
    }

    let force = matches.get_flag("force");
    for loaded in &mut documents {
        for entry in &entries {
            loaded.document.add(entry.clone(), force)?;
        }
    }
    publish_documents(documents, !matches.get_flag("no_backup"))
}

fn remove(matches: &ArgMatches) -> Result<(), Box<dyn Error>> {
    let paths = target_paths(matches)?;
    let mut documents = load_documents(&paths)?;
    let urls: Vec<&String> = matches.get_many::<String>("pack_url").unwrap().collect();
    let force = matches.get_flag("force");
    for loaded in &mut documents {
        for url in &urls {
            loaded.document.remove(url, force)?;
        }
    }
    publish_documents(documents, !matches.get_flag("no_backup"))
}

fn publish_documents(documents: Vec<LoadedDocument>, backup: bool) -> Result<(), Box<dyn Error>> {
    let mut writes = Vec::with_capacity(documents.len());
    let mut changed = Vec::with_capacity(documents.len());
    for loaded in documents {
        let contents = loaded.document.to_bytes();
        changed.push(contents != loaded.original);
        writes.push(DocumentWrite {
            path: loaded.path,
            contents,
            backup,
        });
    }
    validate_document_writes(&writes)?;
    let writes = writes
        .into_iter()
        .zip(changed)
        .filter_map(|(write, changed)| changed.then_some(write))
        .collect();
    commit_documents(writes)?;
    Ok(())
}

fn path_arg(matches: &ArgMatches, name: &str) -> PathBuf {
    PathBuf::from(matches.get_one::<String>(name).unwrap())
}

fn target_paths(matches: &ArgMatches) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut paths = vec![path_arg(matches, "esi_file")];
    if let Some(also) = matches.get_many::<String>("also") {
        paths.extend(also.map(PathBuf::from));
    }
    Ok(paths)
}

struct LoadedDocument {
    path: PathBuf,
    document: Document,
    original: Vec<u8>,
}

fn load_documents(paths: &[PathBuf]) -> Result<Vec<LoadedDocument>, Box<dyn Error>> {
    paths
        .iter()
        .map(|path| {
            let (bytes, document) = load_document(path)?;
            Ok(LoadedDocument {
                path: path.clone(),
                document,
                original: bytes,
            })
        })
        .collect()
}

fn load_document(path: &Path) -> Result<(Vec<u8>, Document), Box<dyn Error>> {
    let bytes =
        fs::read(path).map_err(|error| format!("failed to read '{}': {error}", path.display()))?;
    let document = Document::parse(&bytes)
        .map_err(|error| format!("invalid ESI '{}': {error}", path.display()))?;
    Ok((bytes, document))
}

fn adjacent_index_path(pack: &Path) -> PathBuf {
    let mut path: OsString = pack.as_os_str().to_owned();
    path.push(".idx");
    PathBuf::from(path)
}

fn default_pack_url(pack: &Path) -> Result<String, Box<dyn Error>> {
    let name = pack
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("pack path '{}' is not valid UTF-8", pack.display()))?;
    Ok(encode_url_path_segment(name))
}

fn encode_url_path_segment(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    encoded
}

pub(crate) fn get_app() -> Command {
    Command::new(SUBCOMMAND)
        .subcommand_required(true)
        .arg_required_else_help(true)
        .about("Create and edit elfshaker remote indexes")
        .long_about(
            "Create and edit remote index (.esi) files. A remote index is a small \
             published catalog used by clone and update. It tells clients where \
             to find packs and how to verify them. The v1 format records checksums; \
             v2 also records byte sizes. These commands edit explicitly named \
             local files; they do not use an elfshaker data directory.",
        )
        .arg(
            Arg::new("data_dir")
                .long("data-dir")
                .value_name("PATH")
                .hide(true),
        )
        .subcommand(
            Command::new("create")
                .about("Create an empty remote index")
                .long_about(
                    "Create an empty remote index at ESI_FILE. VERSION selects the \
                     ESI wire format: v1 records pack and pack-index checksums, while \
                     v2 also records their byte sizes. URL is the public URL of the \
                     index and is the base for relative pack URLs.\n\n\
                     The command refuses to replace an existing file unless --force \
                     is present. When it replaces a file, it saves the previous \
                     contents as ESI_FILE.bak unless --no-backup is present.",
                )
                .override_usage("elfshaker esi create [OPTIONS] <ESI_FILE>")
                .arg(
                    Arg::new("meta")
                        .long("meta")
                        .required(true)
                        .value_name("VERSION")
                        .value_parser(["v1", "v2"])
                        .help("ESI wire format: v1 records checksums; v2 also records byte sizes."),
                )
                .arg(
                    Arg::new("url")
                        .long("url")
                        .required(true)
                        .value_name("URL")
                        .help(
                            "Public URL of this index; relative pack URLs are resolved against it.",
                        ),
                )
                .arg(force_arg("Replace an existing ESI file."))
                .arg(no_backup_arg())
                .arg(
                    Arg::new("esi_file")
                        .required(true)
                        .index(1)
                        .value_name("ESI_FILE")
                        .help("ESI file to create."),
                ),
        )
        .subcommand(
            Command::new("list")
                .about("List entries in a remote index")
                .long_about(
                    "Print the entries in ESI_FILE, one per line and in stored order. \
                     A v1 line contains the pack-index checksum, pack checksum, and \
                     pack URL. A v2 line appends the pack size and pack-index size in \
                     bytes. Fields are separated by tabs and no heading is printed.",
                )
                .override_usage("elfshaker esi list <ESI_FILE>")
                .arg(
                    Arg::new("esi_file")
                        .required(true)
                        .index(1)
                        .value_name("ESI_FILE")
                        .help("ESI file to list."),
                ),
        )
        .subcommand(
            Command::new("add")
                .about("Add packs to one or more remote indexes")
                .long_about(
                    "Add each PACK to ESI_FILE and every index named by --also. Each \
                     pack must have an adjacent PACK.idx file. The command reads both \
                     files, calculates their checksums and sizes, and writes the \
                     fields required by each target index version.\n\n\
                     The stored pack URL defaults to the percent-encoded pack \
                     filename. Use --pack-url to provide another URL when adding one \
                     pack. An existing URL is an error unless --force replaces it in \
                     place. All targets are validated before any target changes, and \
                     changed indexes are backed up unless --no-backup is present.",
                )
                .override_usage("elfshaker esi add [OPTIONS] <ESI_FILE> <PACK>...")
                .arg(force_arg("Replace an entry with the same stored URL."))
                .arg(no_backup_arg())
                .arg(also_arg())
                .arg(
                    Arg::new("pack_url")
                        .long("pack-url")
                        .value_name("PACK_URL")
                        .help(
                            "URL to store for one pack; defaults to its percent-encoded filename.",
                        ),
                )
                .arg(
                    Arg::new("esi_file")
                        .required(true)
                        .index(1)
                        .value_name("ESI_FILE")
                        .help("Primary ESI file to edit."),
                )
                .arg(
                    Arg::new("pack")
                        .required(true)
                        .index(2)
                        .num_args(1..)
                        .value_name("PACK")
                        .help("Pack files to add; each must have an adjacent PACK.idx file."),
                ),
        )
        .subcommand(
            Command::new("remove")
                .about("Remove pack URLs from one or more remote indexes")
                .long_about(
                    "Remove entries whose stored URL exactly matches each PACK_URL \
                     from ESI_FILE and every index named by --also. A missing URL is \
                     an error unless --force is present. All targets are validated \
                     before any target changes, and changed indexes are backed up \
                     unless --no-backup is present.",
                )
                .override_usage("elfshaker esi remove [OPTIONS] <ESI_FILE> <PACK_URL>...")
                .arg(force_arg("Ignore stored URLs that are not present."))
                .arg(no_backup_arg())
                .arg(also_arg())
                .arg(
                    Arg::new("esi_file")
                        .required(true)
                        .index(1)
                        .value_name("ESI_FILE")
                        .help("Primary ESI file to edit."),
                )
                .arg(
                    Arg::new("pack_url")
                        .required(true)
                        .index(2)
                        .num_args(1..)
                        .value_name("PACK_URL")
                        .help("Stored pack URLs to remove."),
                ),
        )
}

fn force_arg(help: &'static str) -> Arg {
    Arg::new("force")
        .long("force")
        .action(ArgAction::SetTrue)
        .help(help)
}

fn no_backup_arg() -> Arg {
    Arg::new("no_backup")
        .long("no-backup")
        .action(ArgAction::SetTrue)
        .help("Do not write .bak copies of the previous index contents.")
}

fn also_arg() -> Arg {
    Arg::new("also")
        .long("also")
        .value_name("ESI_FILE")
        .action(ArgAction::Append)
        .help("Apply the same operation to another ESI file; may be repeated.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use elfshaker::esi::Checksum;

    fn temp_dir() -> PathBuf {
        let path = elfshaker::repo::fs::create_temp_path(&std::env::temp_dir());
        fs::create_dir(&path).unwrap();
        path
    }

    fn entry(url: &str) -> Entry {
        Entry {
            index_checksum: Checksum::from([0xaa; 20]),
            pack_checksum: Checksum::from([0xbb; 20]),
            url: url.to_owned(),
            pack_size: Some(12),
            index_size: Some(34),
        }
    }

    #[test]
    fn validates_aliases_before_filtering_unchanged_documents() {
        let dir = temp_dir();
        let primary_path = dir.join("index.esi");
        let secondary_path = dir.join("index.esi.bak");

        let mut primary =
            Document::create(MetaVersion::V1, "https://example.test/index.esi".to_owned()).unwrap();
        primary.add(entry("pack.pack"), false).unwrap();
        let primary_original = primary.to_bytes();
        fs::write(&primary_path, &primary_original).unwrap();
        primary.remove("pack.pack", false).unwrap();

        let secondary = Document::create(
            MetaVersion::V1,
            "https://example.test/index-backup.esi".to_owned(),
        )
        .unwrap();
        let secondary_original = secondary.to_bytes();
        fs::write(&secondary_path, &secondary_original).unwrap();

        let result = publish_documents(
            vec![
                LoadedDocument {
                    path: primary_path.clone(),
                    document: primary,
                    original: primary_original.clone(),
                },
                LoadedDocument {
                    path: secondary_path.clone(),
                    document: secondary,
                    original: secondary_original.clone(),
                },
            ],
            true,
        );

        assert!(result.is_err());
        assert_eq!(fs::read(&primary_path).unwrap(), primary_original);
        assert_eq!(fs::read(&secondary_path).unwrap(), secondary_original);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn default_pack_urls_encode_filename_path_segments() {
        for (name, expected) in [
            ("hash#name.pack", "hash%23name.pack"),
            ("release:debug.pack", "release%3Adebug.pack"),
            ("pack with-space.pack", "pack%20with-space.pack"),
        ] {
            let stored = default_pack_url(Path::new(name)).unwrap();
            assert_eq!(stored, expected);
            let resolved = url::Url::parse("https://example.test/releases/index.esi")
                .unwrap()
                .join(&stored)
                .unwrap();
            assert_eq!(resolved.scheme(), "https");
            assert_eq!(resolved.fragment(), None);
            assert_eq!(resolved.path(), format!("/releases/{expected}"));
        }
    }
}
