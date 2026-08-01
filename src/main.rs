use clap::{Parser, Subcommand, ValueEnum, error::ErrorKind};
use pklx::pklr::{EvalOptions, Value};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::UNIX_EPOCH;

const SCHEMA_VERSION: i64 = 1;
const EMBEDDED_CONFIG: &str = include_str!("../pkl/Config.pkl");
type Result<T> = std::result::Result<T, String>;

#[derive(Parser)]
#[command(
    name = "marchiver",
    about = "Portable CLI for quality-conscious media archiving and migration",
    disable_version_flag = true
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Inspect and hash one regular media file.
    Inspect {
        source: PathBuf,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Produce a trusted, self-contained Pkl archive plan.
    Plan {
        source: PathBuf,
        /// Final destination file path. Directory destinations are not inferred.
        destination: PathBuf,
        #[arg(long, value_enum)]
        profile: Option<Profile>,
        /// Trusted local Pkl configuration. Never use untrusted Pkl input.
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long, required = true)]
        output: PathBuf,
    },
    /// Execute or resume one trusted Pkl archive plan.
    Apply { plan: PathBuf },
    /// Verify an archived output from its trusted Pkl manifest.
    Verify { manifest: PathBuf },
    /// Restore a quarantined source from its trusted Pkl manifest.
    Restore { manifest: PathBuf },
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "lowercase")]
enum Profile {
    Copy,
    Av1,
}

impl Profile {
    fn as_str(self) -> &'static str {
        match self {
            Self::Copy => "copy",
            Self::Av1 => "av1",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum SourceDisposition {
    Quarantine,
    Preserve,
    Delete,
}

impl SourceDisposition {
    fn as_str(self) -> &'static str {
        match self {
            Self::Quarantine => "quarantine",
            Self::Preserve => "preserve",
            Self::Delete => "delete",
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Av1Config {
    crf: u8,
    preset: u8,
    container: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Config {
    schema_version: i64,
    source_disposition: SourceDisposition,
    default_profile: Profile,
    av1: Av1Config,
    duration_tolerance_seconds: f64,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MediaEvidence {
    path: String,
    sha256: String,
    size: u64,
    modified_ns: i64,
    format: FormatSummary,
    streams: Vec<StreamSummary>,
    chapters: Vec<ChapterSummary>,
    raw_probe_json: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FormatSummary {
    format_name: Option<String>,
    duration_seconds: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StreamSummary {
    index: u32,
    codec_name: Option<String>,
    codec_type: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    duration_seconds: Option<f64>,
    bits_per_raw_sample: Option<String>,
    pixel_format: Option<String>,
    color_space: Option<String>,
    color_transfer: Option<String>,
    color_primaries: Option<String>,
    attached_picture: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChapterSummary {
    start_seconds: f64,
    end_seconds: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ArchiveInspection {
    schema_version: i64,
    kind: String,
    producer_version: String,
    evidence: MediaEvidence,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ArchivePlan {
    schema_version: i64,
    kind: String,
    producer_version: String,
    transaction_id: String,
    profile: Profile,
    source_disposition: SourceDisposition,
    duration_tolerance_seconds: f64,
    av1: Av1Config,
    source: MediaEvidence,
    destination_path: String,
    manifest_path: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ArchiveManifest {
    schema_version: i64,
    kind: String,
    producer_version: String,
    transaction_id: String,
    profile: Profile,
    source_disposition: SourceDisposition,
    duration_tolerance_seconds: f64,
    av1: Av1Config,
    source: MediaEvidence,
    #[serde(rename = "outputEvidence")]
    output: MediaEvidence,
    destination_path: String,
    manifest_path: String,
    quarantine_path: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
enum Phase {
    Prepared,
    Staged,
    Published,
    Verified,
    Manifested,
    QuarantineLinked,
    Complete,
}

impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Staged => "staged",
            Self::Published => "published",
            Self::Verified => "verified",
            Self::Manifested => "manifested",
            Self::QuarantineLinked => "quarantineLinked",
            Self::Complete => "complete",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Journal {
    schema_version: i64,
    kind: String,
    producer_version: String,
    transaction_id: String,
    source_path: String,
    destination_path: String,
    manifest_path: String,
    staging_path: String,
    phase: Phase,
    #[serde(rename = "outputEvidence")]
    output: Option<MediaEvidence>,
    quarantine_path: Option<String>,
    diagnostic: Option<String>,
}

#[derive(Deserialize)]
struct Probe {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    #[serde(default)]
    chapters: Vec<ProbeChapter>,
    #[serde(default)]
    format: ProbeFormat,
}

#[derive(Default, Deserialize)]
struct ProbeFormat {
    format_name: Option<String>,
    duration: Option<String>,
}

#[derive(Deserialize)]
struct ProbeStream {
    index: u32,
    codec_name: Option<String>,
    codec_type: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    duration: Option<String>,
    bits_per_raw_sample: Option<String>,
    pix_fmt: Option<String>,
    color_space: Option<String>,
    color_transfer: Option<String>,
    color_primaries: Option<String>,
    #[serde(default)]
    disposition: ProbeDisposition,
}

#[derive(Default, Deserialize)]
struct ProbeDisposition {
    #[serde(default)]
    attached_pic: i32,
}

#[derive(Deserialize)]
struct ProbeChapter {
    start_time: String,
    end_time: String,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().collect();
    if args.len() == 1 || matches_single_flag(&args, "-h", "--help") {
        print_root_help();
        return ExitCode::SUCCESS;
    }
    if matches_single_flag(&args, "-V", "--version") {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if args.len() > 2 && matches!(args[1].to_str(), Some("-h" | "--help" | "-V" | "--version")) {
        eprintln!("unknown arguments; try `marchiver --help`");
        return ExitCode::from(2);
    }

    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) if error.kind() == ErrorKind::DisplayHelp => {
            print!("{error}");
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprint!("{error}");
            eprintln!("try `marchiver --help`");
            return ExitCode::from(2);
        }
    };

    match run(cli.command).await {
        Ok(message) => {
            if let Some(message) = message {
                println!("{message}");
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("marchiver: {error}");
            ExitCode::FAILURE
        }
    }
}

fn matches_single_flag(args: &[OsString], short: &str, long: &str) -> bool {
    args.len() == 2 && (args[1] == OsStr::new(short) || args[1] == OsStr::new(long))
}

fn print_root_help() {
    println!(
        "marchiver {}\n\nPortable CLI for quality-conscious media archiving and migration.\n\nUsage:\n  marchiver [OPTIONS]\n  marchiver <COMMAND>\n\nCommands:\n  inspect  Inspect and hash one regular media file\n  plan     Produce a trusted, self-contained Pkl archive plan\n  apply    Execute or resume one trusted Pkl archive plan\n  verify   Verify an archived output from its trusted Pkl manifest\n  restore  Restore a quarantined source from its trusted Pkl manifest\n\nOptions:\n  -h, --help       Print help\n  -V, --version    Print version",
        env!("CARGO_PKG_VERSION")
    );
}

async fn run(command: Commands) -> Result<Option<String>> {
    match command {
        Commands::Inspect { source, output } => {
            let inspection = ArchiveInspection {
                schema_version: SCHEMA_VERSION,
                kind: "ArchiveInspection".into(),
                producer_version: env!("CARGO_PKG_VERSION").into(),
                evidence: inspect_file(&source)?,
            };
            let rendered = render_inspection(&inspection);
            if let Some(output) = output {
                write_user_output(&output, rendered.as_bytes())?;
                Ok(Some(output.display().to_string()))
            } else {
                print!("{rendered}");
                Ok(None)
            }
        }
        Commands::Plan {
            source,
            destination,
            profile,
            config,
            output,
        } => {
            let config = load_config(config.as_deref()).await?;
            let profile = profile.unwrap_or(config.default_profile);
            let plan = make_plan(&source, &destination, profile, config)?;
            write_user_output(&output, render_plan(&plan).as_bytes())?;
            Ok(Some(output.display().to_string()))
        }
        Commands::Apply { plan } => apply(&plan).await.map(Some),
        Commands::Verify { manifest } => {
            let manifest = load_manifest(&manifest).await?;
            verify_manifest(&manifest)?;
            Ok(Some(format!("verified {}", manifest.destination_path)))
        }
        Commands::Restore { manifest } => {
            let manifest = load_manifest(&manifest).await?;
            restore(&manifest)?;
            Ok(Some(format!("restored {}", manifest.source.path)))
        }
    }
}

async fn load_config(path: Option<&Path>) -> Result<Config> {
    let value = if let Some(path) = path {
        pklx::eval_to_value(path, EvalOptions::default())
            .await
            .map_err(|error| {
                format!(
                    "failed to evaluate trusted config '{}': {error}",
                    path.display()
                )
            })?
    } else {
        pklx::eval_source_to_value(EMBEDDED_CONFIG, EvalOptions::default())
            .await
            .map_err(|error| format!("failed to evaluate embedded config: {error}"))?
    };
    require_schema(&value, "Config")?;
    let config: Config =
        pklx::from_pkl_value(&value).map_err(|error| format!("invalid Config Pkl: {error}"))?;
    validate_config(&config)?;
    Ok(config)
}

async fn load_plan(path: &Path) -> Result<ArchivePlan> {
    let value = eval_document(path, "ArchivePlan").await?;
    let plan: ArchivePlan = pklx::from_pkl_value(&value)
        .map_err(|error| format!("invalid ArchivePlan Pkl: {error}"))?;
    validate_plan(&plan)?;
    Ok(plan)
}

async fn load_manifest(path: &Path) -> Result<ArchiveManifest> {
    let value = eval_document(path, "ArchiveManifest").await?;
    let manifest: ArchiveManifest = pklx::from_pkl_value(&value)
        .map_err(|error| format!("invalid ArchiveManifest Pkl: {error}"))?;
    validate_manifest(&manifest, Some(path))?;
    Ok(manifest)
}

async fn load_journal(path: &Path) -> Result<Journal> {
    let value = eval_document(path, "ArchiveJournal").await?;
    let journal: Journal = pklx::from_pkl_value(&value)
        .map_err(|error| format!("invalid ArchiveJournal Pkl: {error}"))?;
    if journal.schema_version != SCHEMA_VERSION || journal.kind != "ArchiveJournal" {
        return Err("invalid archive journal identity".into());
    }
    Ok(journal)
}

async fn eval_document(path: &Path, expected_kind: &str) -> Result<Value> {
    let value = pklx::eval_to_value(path, EvalOptions::default())
        .await
        .map_err(|error| {
            format!(
                "failed to evaluate trusted Pkl '{}': {error}",
                path.display()
            )
        })?;
    require_schema(&value, expected_kind)?;
    let kind = object_field(&value, "kind")?;
    if !matches!(kind, Value::String(kind) if kind == expected_kind) {
        return Err(format!("expected {expected_kind} Pkl document"));
    }
    Ok(value)
}

fn require_schema(value: &Value, document: &str) -> Result<()> {
    match object_field(value, "schemaVersion")? {
        Value::Int(SCHEMA_VERSION) => Ok(()),
        Value::Int(version) => Err(format!(
            "unsupported {document} schemaVersion {version}; expected {SCHEMA_VERSION}"
        )),
        _ => Err(format!("{document} schemaVersion must be an integer")),
    }
}

fn object_field<'a>(value: &'a Value, field: &str) -> Result<&'a Value> {
    match value {
        Value::Object(fields, _) => fields
            .get(field)
            .ok_or_else(|| format!("Pkl document is missing {field}")),
        _ => Err("Pkl document must evaluate to an object".into()),
    }
}

fn validate_config(config: &Config) -> Result<()> {
    if config.schema_version != SCHEMA_VERSION {
        return Err("config schemaVersion changed after raw validation".into());
    }
    if config.av1.container != "mkv" {
        return Err("AV1 container must be mkv".into());
    }
    if config.av1.crf > 63 || config.av1.preset > 13 {
        return Err("AV1 crf must be 0..=63 and preset must be 0..=13".into());
    }
    if !config.duration_tolerance_seconds.is_finite() || config.duration_tolerance_seconds < 0.0 {
        return Err("durationToleranceSeconds must be finite and non-negative".into());
    }
    Ok(())
}

fn make_plan(
    source: &Path,
    destination: &Path,
    profile: Profile,
    config: Config,
) -> Result<ArchivePlan> {
    let source = inspect_file(source)?;
    let destination = final_destination(destination)?;
    let destination_string = path_string(&destination)?;
    if source.path == destination_string {
        return Err("source and destination must differ".into());
    }
    if profile == Profile::Av1 && destination.extension().and_then(OsStr::to_str) != Some("mkv") {
        return Err("AV1 destination must use the .mkv extension".into());
    }
    let manifest_path = destination.with_file_name(format!(
        "{}.marchiver.pkl",
        destination
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| "destination file name must be UTF-8".to_string())?
    ));
    let mut plan = ArchivePlan {
        schema_version: SCHEMA_VERSION,
        kind: "ArchivePlan".into(),
        producer_version: env!("CARGO_PKG_VERSION").into(),
        transaction_id: String::new(),
        profile,
        source_disposition: config.source_disposition,
        duration_tolerance_seconds: config.duration_tolerance_seconds,
        av1: config.av1,
        source,
        destination_path: destination_string,
        manifest_path: path_string(&manifest_path)?,
    };
    plan.transaction_id = transaction_id(&plan);
    validate_plan(&plan)?;
    Ok(plan)
}

fn final_destination(path: &Path) -> Result<PathBuf> {
    if path.file_name().is_none() {
        return Err("destination must be a final file path".into());
    }
    if path.exists() {
        return Err(format!("destination already exists: {}", path.display()));
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = parent
        .canonicalize()
        .map_err(|error| format!("invalid destination parent '{}': {error}", parent.display()))?;
    if !parent.is_dir() {
        return Err("destination parent is not a directory".into());
    }
    Ok(parent.join(path.file_name().expect("checked above")))
}

fn transaction_id(plan: &ArchivePlan) -> String {
    let mut hash = Sha256::new();
    for value in [
        plan.source.sha256.as_str(),
        plan.source.path.as_str(),
        plan.destination_path.as_str(),
        plan.profile.as_str(),
        plan.source_disposition.as_str(),
        plan.av1.container.as_str(),
    ] {
        hash.update(value.as_bytes());
        hash.update([0]);
    }
    hash.update(plan.av1.crf.to_le_bytes());
    hash.update(plan.av1.preset.to_le_bytes());
    hash.update(plan.duration_tolerance_seconds.to_le_bytes());
    hex_digest(hash.finalize())[..24].to_string()
}

fn validate_plan(plan: &ArchivePlan) -> Result<()> {
    if plan.schema_version != SCHEMA_VERSION
        || plan.kind != "ArchivePlan"
        || plan.producer_version.is_empty()
    {
        return Err("invalid archive plan identity".into());
    }
    validate_settings(&plan.av1, plan.duration_tolerance_seconds)?;
    if plan.transaction_id != transaction_id(plan) {
        return Err("archive plan transactionId does not match its contents".into());
    }
    let source = Path::new(&plan.source.path);
    let destination = Path::new(&plan.destination_path);
    if !source.is_absolute() || !destination.is_absolute() || source == destination {
        return Err("plan paths must be distinct absolute paths".into());
    }
    if Path::new(&plan.manifest_path).parent() != destination.parent() {
        return Err("manifest must be adjacent to destination".into());
    }
    if plan.profile == Profile::Av1 && destination.extension() != Some(OsStr::new("mkv")) {
        return Err("AV1 destination must use the .mkv extension".into());
    }
    validate_hash(&plan.source.sha256)
}

fn validate_manifest(manifest: &ArchiveManifest, loaded_from: Option<&Path>) -> Result<()> {
    if manifest.schema_version != SCHEMA_VERSION || manifest.kind != "ArchiveManifest" {
        return Err("invalid archive manifest identity".into());
    }
    validate_settings(&manifest.av1, manifest.duration_tolerance_seconds)?;
    validate_hash(&manifest.source.sha256)?;
    validate_hash(&manifest.output.sha256)?;
    if manifest.output.path != manifest.destination_path {
        return Err("manifest output path does not match destinationPath".into());
    }
    let transaction_plan = ArchivePlan {
        schema_version: SCHEMA_VERSION,
        kind: "ArchivePlan".into(),
        producer_version: manifest.producer_version.clone(),
        transaction_id: manifest.transaction_id.clone(),
        profile: manifest.profile,
        source_disposition: manifest.source_disposition,
        duration_tolerance_seconds: manifest.duration_tolerance_seconds,
        av1: manifest.av1.clone(),
        source: manifest.source.clone(),
        destination_path: manifest.destination_path.clone(),
        manifest_path: manifest.manifest_path.clone(),
    };
    validate_plan(&transaction_plan)?;
    if manifest.quarantine_path != quarantine_path(&transaction_plan)? {
        return Err("manifest quarantinePath does not match its transaction".into());
    }
    if let Some(path) = loaded_from {
        let loaded = path
            .canonicalize()
            .map_err(|error| format!("cannot resolve manifest '{}': {error}", path.display()))?;
        let declared = Path::new(&manifest.manifest_path)
            .canonicalize()
            .map_err(|error| format!("cannot resolve declared manifest path: {error}"))?;
        if loaded != declared {
            return Err("manifestPath does not identify the loaded manifest".into());
        }
    }
    Ok(())
}

fn validate_settings(av1: &Av1Config, tolerance: f64) -> Result<()> {
    if av1.container != "mkv" || av1.crf > 63 || av1.preset > 13 {
        return Err("invalid AV1 settings".into());
    }
    if !tolerance.is_finite() || tolerance < 0.0 {
        return Err("duration tolerance must be finite and non-negative".into());
    }
    Ok(())
}

fn validate_hash(hash: &str) -> Result<()> {
    if hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err("SHA-256 value must contain 64 hexadecimal characters".into())
    }
}

fn inspect_file(path: &Path) -> Result<MediaEvidence> {
    let canonical = regular_file(path)?;
    let before = fs::metadata(&canonical)
        .map_err(|error| format!("cannot stat '{}': {error}", canonical.display()))?;
    let sha256 = hash_file(&canonical)?;
    let raw_probe_json = ffprobe(&canonical)?;
    let probe: Probe = serde_json::from_str(&raw_probe_json)
        .map_err(|error| format!("ffprobe returned invalid JSON: {error}"))?;
    let after = fs::metadata(&canonical)
        .map_err(|error| format!("cannot restat '{}': {error}", canonical.display()))?;
    if !same_metadata(&before, &after)? {
        return Err(format!(
            "source changed while inspecting: {}",
            canonical.display()
        ));
    }
    let modified_ns = modified_ns(&after)?;
    Ok(MediaEvidence {
        path: path_string(&canonical)?,
        sha256,
        size: after.len(),
        modified_ns,
        format: FormatSummary {
            format_name: probe.format.format_name,
            duration_seconds: parse_optional_time(
                probe.format.duration.as_deref(),
                "format duration",
            )?,
        },
        streams: probe
            .streams
            .into_iter()
            .map(|stream| {
                Ok(StreamSummary {
                    index: stream.index,
                    codec_name: stream.codec_name,
                    codec_type: stream.codec_type,
                    width: stream.width,
                    height: stream.height,
                    duration_seconds: parse_optional_time(
                        stream.duration.as_deref(),
                        "stream duration",
                    )?,
                    bits_per_raw_sample: stream.bits_per_raw_sample,
                    pixel_format: stream.pix_fmt,
                    color_space: stream.color_space,
                    color_transfer: stream.color_transfer,
                    color_primaries: stream.color_primaries,
                    attached_picture: stream.disposition.attached_pic != 0,
                })
            })
            .collect::<Result<Vec<_>>>()?,
        chapters: probe
            .chapters
            .into_iter()
            .map(|chapter| {
                Ok(ChapterSummary {
                    start_seconds: parse_time(&chapter.start_time, "chapter start")?,
                    end_seconds: parse_time(&chapter.end_time, "chapter end")?,
                })
            })
            .collect::<Result<Vec<_>>>()?,
        raw_probe_json,
    })
}

fn regular_file(path: &Path) -> Result<PathBuf> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect '{}': {error}", path.display()))?;
    if !metadata.file_type().is_file() {
        return Err(format!("not a regular file: {}", path.display()));
    }
    path.canonicalize()
        .map_err(|error| format!("cannot resolve '{}': {error}", path.display()))
}

fn hash_file(path: &Path) -> Result<String> {
    let file =
        File::open(path).map_err(|error| format!("cannot open '{}': {error}", path.display()))?;
    let mut reader = BufReader::with_capacity(1024 * 1024, file);
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("cannot hash '{}': {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(hex_digest(hash.finalize()))
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn modified_ns(metadata: &fs::Metadata) -> Result<i64> {
    let nanos = metadata
        .modified()
        .map_err(|error| format!("cannot read modification time: {error}"))?
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "file modification time predates Unix epoch".to_string())?
        .as_nanos();
    i64::try_from(nanos).map_err(|_| "file modification time is out of range".into())
}

#[cfg(unix)]
fn same_metadata(left: &fs::Metadata, right: &fs::Metadata) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;
    Ok(left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.len() == right.len()
        && modified_ns(left)? == modified_ns(right)?)
}

#[cfg(not(unix))]
fn same_metadata(left: &fs::Metadata, right: &fs::Metadata) -> Result<bool> {
    Ok(left.len() == right.len() && modified_ns(left)? == modified_ns(right)?)
}

fn ffprobe(path: &Path) -> Result<String> {
    let output = Command::new("ffprobe")
        .args([
            OsStr::new("-v"),
            OsStr::new("error"),
            OsStr::new("-show_format"),
            OsStr::new("-show_streams"),
            OsStr::new("-show_chapters"),
            OsStr::new("-of"),
            OsStr::new("json"),
        ])
        .arg(path)
        .output()
        .map_err(|error| format!("failed to execute ffprobe: {error}"))?;
    if !output.status.success() {
        return Err(command_failure("ffprobe", &output.stderr));
    }
    String::from_utf8(output.stdout)
        .map_err(|error| format!("ffprobe output is not UTF-8: {error}"))
}

fn parse_optional_time(value: Option<&str>, name: &str) -> Result<Option<f64>> {
    value.map(|value| parse_time(value, name)).transpose()
}

fn parse_time(value: &str, name: &str) -> Result<f64> {
    let value = value
        .parse::<f64>()
        .map_err(|error| format!("invalid {name} '{value}': {error}"))?;
    if value.is_finite() && value >= 0.0 {
        Ok(value)
    } else {
        Err(format!("invalid {name}: must be finite and non-negative"))
    }
}

fn validate_source(expected: &MediaEvidence) -> Result<()> {
    let actual = inspect_file(Path::new(&expected.path))?;
    if actual.sha256 != expected.sha256
        || actual.size != expected.size
        || actual.modified_ns != expected.modified_ns
        || actual.format != expected.format
        || actual.streams != expected.streams
        || actual.chapters != expected.chapters
    {
        return Err(format!("source changed since planning: {}", expected.path));
    }
    Ok(())
}

async fn apply(plan_path: &Path) -> Result<String> {
    // ponytail: one process per transaction; add an OS advisory lock if concurrent applies matter.
    let plan = load_plan(plan_path).await?;
    let destination = PathBuf::from(&plan.destination_path);
    let manifest_path = PathBuf::from(&plan.manifest_path);
    let destination_parent = destination
        .parent()
        .ok_or_else(|| "destination has no parent".to_string())?;
    let transaction_root = destination_parent.join(".marchiver-transactions");
    ensure_directory(&transaction_root)?;
    let transaction_dir = transaction_root.join(&plan.transaction_id);
    ensure_directory(&transaction_dir)?;
    let journal_path = transaction_dir.join("journal.pkl");
    let staging_path = transaction_dir.join("output.partial");

    let mut journal = if journal_path.exists() {
        let journal = load_journal(&journal_path).await?;
        validate_journal(&journal, &plan, &staging_path)?;
        journal
    } else {
        if destination.exists() || manifest_path.exists() {
            return Err("destination or manifest already exists; refusing overwrite".into());
        }
        let journal = Journal {
            schema_version: SCHEMA_VERSION,
            kind: "ArchiveJournal".into(),
            producer_version: env!("CARGO_PKG_VERSION").into(),
            transaction_id: plan.transaction_id.clone(),
            source_path: plan.source.path.clone(),
            destination_path: plan.destination_path.clone(),
            manifest_path: plan.manifest_path.clone(),
            staging_path: path_string(&staging_path)?,
            phase: Phase::Prepared,
            output: None,
            quarantine_path: None,
            diagnostic: None,
        };
        write_journal(&journal_path, &journal)?;
        journal
    };

    if journal.phase == Phase::Complete {
        return Ok(format!("already complete {}", plan.manifest_path));
    }

    if journal.phase == Phase::Prepared {
        if let Err(error) = validate_source(&plan.source) {
            journal.diagnostic = Some(error.clone());
            write_journal(&journal_path, &journal)?;
            return Err(error);
        }
        if staging_path.exists() {
            remove_internal_regular(&staging_path)?;
        }
        let stage_result = match plan.profile {
            Profile::Copy => copy_stage(Path::new(&plan.source.path), &staging_path),
            Profile::Av1 => av1_stage(&plan, &staging_path),
        };
        if let Err(error) = stage_result {
            journal.diagnostic = Some(error.clone());
            write_journal(&journal_path, &journal)?;
            return Err(error);
        }
        let mut output = match inspect_file(&staging_path) {
            Ok(output) => output,
            Err(error) => {
                journal.diagnostic = Some(error.clone());
                write_journal(&journal_path, &journal)?;
                return Err(error);
            }
        };
        output.path.clone_from(&plan.destination_path);
        journal.output = Some(output);
        journal.phase = Phase::Staged;
        journal.diagnostic = None;
        write_journal(&journal_path, &journal)?;
    }

    if journal.phase == Phase::Staged {
        let expected = journal
            .output
            .as_ref()
            .ok_or_else(|| "staged journal is missing output evidence".to_string())?;
        publish_output(&staging_path, &destination, expected)?;
        journal.phase = Phase::Published;
        write_journal(&journal_path, &journal)?;
    }

    if journal.phase == Phase::Published {
        let output = match inspect_file(&destination) {
            Ok(output) => output,
            Err(error) => {
                journal.diagnostic = Some(error.clone());
                write_journal(&journal_path, &journal)?;
                return Err(error);
            }
        };
        if output.sha256
            != journal
                .output
                .as_ref()
                .ok_or_else(|| "published journal is missing output evidence".to_string())?
                .sha256
        {
            return Err("published output differs from staged output".into());
        }
        let verification = verify_output(
            plan.profile,
            &plan.source,
            &output,
            plan.duration_tolerance_seconds,
        )
        .and_then(|()| decode_output(&destination));
        if let Err(error) = verification {
            journal.diagnostic = Some(error.clone());
            write_journal(&journal_path, &journal)?;
            return Err(error);
        }
        journal.output = Some(output);
        journal.phase = Phase::Verified;
        journal.diagnostic = None;
        write_journal(&journal_path, &journal)?;
    }

    if journal.phase == Phase::Verified {
        let quarantine_path = quarantine_path(&plan)?;
        let manifest = manifest_from_plan(
            &plan,
            journal
                .output
                .clone()
                .ok_or_else(|| "verified journal is missing output evidence".to_string())?,
            quarantine_path.clone(),
        );
        if manifest_path.exists() {
            let existing = load_manifest(&manifest_path).await?;
            if existing.transaction_id != manifest.transaction_id
                || existing.output.sha256 != manifest.output.sha256
            {
                return Err("existing manifest belongs to another transaction".into());
            }
        } else {
            publish_text_no_replace(
                &transaction_dir.join("manifest.partial"),
                &manifest_path,
                render_manifest(&manifest).as_bytes(),
            )?;
        }
        journal.quarantine_path = quarantine_path;
        journal.phase = Phase::Manifested;
        write_journal(&journal_path, &journal)?;
    }

    if journal.phase == Phase::Manifested {
        match plan.source_disposition {
            SourceDisposition::Preserve => {
                journal.phase = Phase::Complete;
                write_journal(&journal_path, &journal)?;
            }
            SourceDisposition::Delete => {
                let source = Path::new(&plan.source.path);
                if source.exists() {
                    validate_source(&plan.source)?;
                    fs::remove_file(source)
                        .map_err(|error| format!("failed to delete source: {error}"))?;
                    sync_parent(source)?;
                }
                journal.phase = Phase::Complete;
                write_journal(&journal_path, &journal)?;
            }
            SourceDisposition::Quarantine => {
                quarantine_link(&plan, &journal)?;
                journal.phase = Phase::QuarantineLinked;
                write_journal(&journal_path, &journal)?;
            }
        }
    }

    if journal.phase == Phase::QuarantineLinked {
        finish_quarantine(&plan, &journal)?;
        journal.phase = Phase::Complete;
        write_journal(&journal_path, &journal)?;
    }

    Ok(plan.manifest_path)
}

fn validate_journal(journal: &Journal, plan: &ArchivePlan, staging: &Path) -> Result<()> {
    if journal.schema_version != SCHEMA_VERSION
        || journal.kind != "ArchiveJournal"
        || journal.producer_version.is_empty()
        || journal.transaction_id != plan.transaction_id
        || journal.source_path != plan.source.path
        || journal.destination_path != plan.destination_path
        || journal.manifest_path != plan.manifest_path
        || journal.staging_path != path_string(staging)?
    {
        return Err("journal does not match archive plan".into());
    }
    if journal.phase >= Phase::Staged && journal.output.is_none() {
        return Err("journal phase requires output evidence".into());
    }
    if journal.phase >= Phase::QuarantineLinked
        && plan.source_disposition == SourceDisposition::Quarantine
        && journal.quarantine_path.is_none()
    {
        return Err("journal phase requires quarantinePath".into());
    }
    Ok(())
}

fn copy_stage(source: &Path, staging: &Path) -> Result<()> {
    let mut input = File::open(source)
        .map_err(|error| format!("cannot open source '{}': {error}", source.display()))?;
    let output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(staging)
        .map_err(|error| {
            format!(
                "cannot create staging file '{}': {error}",
                staging.display()
            )
        })?;
    let mut output = BufWriter::with_capacity(1024 * 1024, output);
    io::copy(&mut input, &mut output)
        .map_err(|error| format!("copy to staging failed: {error}"))?;
    output
        .flush()
        .map_err(|error| format!("cannot flush staging output: {error}"))?;
    output
        .get_ref()
        .sync_all()
        .map_err(|error| format!("cannot sync staging output: {error}"))?;
    Ok(())
}

fn av1_stage(plan: &ArchivePlan, staging: &Path) -> Result<()> {
    let mut command = Command::new("ffmpeg");
    command.args(["-v", "error", "-nostdin", "-n", "-i"]);
    command.arg(&plan.source.path);
    command.args([
        "-map",
        "0",
        "-map_metadata",
        "0",
        "-map_chapters",
        "0",
        "-c",
        "copy",
    ]);
    let mut video_ordinal = 0_u32;
    for stream in &plan.source.streams {
        if stream.codec_type.as_deref() != Some("video") {
            continue;
        }
        if !stream.attached_picture {
            command.args([
                format!("-c:v:{video_ordinal}"),
                "libsvtav1".into(),
                format!("-crf:v:{video_ordinal}"),
                plan.av1.crf.to_string(),
                format!("-preset:v:{video_ordinal}"),
                plan.av1.preset.to_string(),
            ]);
            add_stream_option(&mut command, "pix_fmt", video_ordinal, &stream.pixel_format);
            add_stream_option(
                &mut command,
                "color_primaries",
                video_ordinal,
                &stream.color_primaries,
            );
            add_stream_option(
                &mut command,
                "color_trc",
                video_ordinal,
                &stream.color_transfer,
            );
            add_stream_option(
                &mut command,
                "colorspace",
                video_ordinal,
                &stream.color_space,
            );
        }
        video_ordinal += 1;
    }
    command.args(["-f", "matroska"]);
    command.arg(staging);
    let output = command
        .output()
        .map_err(|error| format!("failed to execute ffmpeg: {error}"))?;
    if !output.status.success() {
        return Err(command_failure("ffmpeg transcode", &output.stderr));
    }
    let staged = File::open(staging)
        .map_err(|error| format!("ffmpeg did not create staging output: {error}"))?;
    staged
        .sync_all()
        .map_err(|error| format!("cannot sync staging output: {error}"))
}

fn add_stream_option(command: &mut Command, name: &str, ordinal: u32, value: &Option<String>) {
    if let Some(value) = value {
        command.arg(format!("-{name}:v:{ordinal}")).arg(value);
    }
}

fn command_failure(command: &str, stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let stderr = stderr.trim();
    if stderr.is_empty() {
        format!("{command} failed without diagnostics")
    } else {
        format!("{command} failed: {stderr}")
    }
}

fn publish_output(staging: &Path, destination: &Path, expected: &MediaEvidence) -> Result<()> {
    if destination.exists() {
        let actual = regular_file(destination)?;
        if hash_file(&actual)? == expected.sha256 {
            if staging.exists() {
                remove_internal_regular(staging)?;
                sync_parent(staging)?;
            }
            return Ok(());
        }
        return Err("destination already exists and is not this transaction's output".into());
    }
    fs::hard_link(staging, destination).map_err(|error| {
        format!(
            "atomic no-replace publication '{}' failed: {error}",
            destination.display()
        )
    })?;
    sync_parent(destination)?;
    fs::remove_file(staging)
        .map_err(|error| format!("cannot remove published staging link: {error}"))?;
    sync_parent(staging)
}

fn manifest_from_plan(
    plan: &ArchivePlan,
    output: MediaEvidence,
    quarantine_path: Option<String>,
) -> ArchiveManifest {
    ArchiveManifest {
        schema_version: SCHEMA_VERSION,
        kind: "ArchiveManifest".into(),
        producer_version: env!("CARGO_PKG_VERSION").into(),
        transaction_id: plan.transaction_id.clone(),
        profile: plan.profile,
        source_disposition: plan.source_disposition,
        duration_tolerance_seconds: plan.duration_tolerance_seconds,
        av1: plan.av1.clone(),
        source: plan.source.clone(),
        output,
        destination_path: plan.destination_path.clone(),
        manifest_path: plan.manifest_path.clone(),
        quarantine_path,
    }
}

fn quarantine_path(plan: &ArchivePlan) -> Result<Option<String>> {
    if plan.source_disposition != SourceDisposition::Quarantine {
        return Ok(None);
    }
    let source = Path::new(&plan.source.path);
    let name = source
        .file_name()
        .ok_or_else(|| "source has no file name".to_string())?;
    let path = source
        .parent()
        .ok_or_else(|| "source has no parent".to_string())?
        .join(".marchiver-quarantine")
        .join(&plan.transaction_id)
        .join(name);
    Ok(Some(path_string(&path)?))
}

fn quarantine_link(plan: &ArchivePlan, journal: &Journal) -> Result<()> {
    let source = Path::new(&plan.source.path);
    let quarantine = Path::new(
        journal
            .quarantine_path
            .as_deref()
            .ok_or_else(|| "quarantine journal is missing quarantinePath".to_string())?,
    );
    let transaction_dir = quarantine
        .parent()
        .ok_or_else(|| "quarantine path has no parent".to_string())?;
    let root = transaction_dir
        .parent()
        .ok_or_else(|| "quarantine path has no root".to_string())?;
    ensure_directory(root)?;
    ensure_directory(transaction_dir)?;

    match (source.exists(), quarantine.exists()) {
        (true, false) => {
            validate_source(&plan.source)?;
            fs::hard_link(source, quarantine).map_err(|error| {
                format!("quarantine hard-link failed (cross-device fallback is forbidden): {error}")
            })?;
            File::open(quarantine)
                .and_then(|file| file.sync_all())
                .map_err(|error| format!("cannot sync quarantine link: {error}"))?;
            sync_parent(quarantine)
        }
        (true, true) => ensure_same_file(source, quarantine),
        (false, true) => Ok(()),
        (false, false) => Err("both source and quarantine are missing".into()),
    }
}

fn finish_quarantine(plan: &ArchivePlan, journal: &Journal) -> Result<()> {
    let source = Path::new(&plan.source.path);
    let quarantine = Path::new(
        journal
            .quarantine_path
            .as_deref()
            .ok_or_else(|| "quarantine journal is missing quarantinePath".to_string())?,
    );
    if source.exists() {
        if !quarantine.exists() {
            return Err("quarantine link disappeared before source unlink".into());
        }
        ensure_same_file(source, quarantine)?;
        fs::remove_file(source).map_err(|error| format!("cannot unlink source: {error}"))?;
        sync_parent(source)?;
    } else if !quarantine.exists() {
        return Err("both source and quarantine are missing".into());
    }
    Ok(())
}

fn verify_manifest(manifest: &ArchiveManifest) -> Result<()> {
    let output = inspect_file(Path::new(&manifest.destination_path))?;
    if output.sha256 != manifest.output.sha256 {
        return Err("output SHA-256 differs from manifest".into());
    }
    verify_output(
        manifest.profile,
        &manifest.source,
        &output,
        manifest.duration_tolerance_seconds,
    )?;
    decode_output(Path::new(&manifest.destination_path))
}

fn verify_output(
    profile: Profile,
    source: &MediaEvidence,
    output: &MediaEvidence,
    tolerance: f64,
) -> Result<()> {
    if output.size == 0 {
        return Err("output is empty".into());
    }
    if profile == Profile::Copy && output.sha256 != source.sha256 {
        return Err("copy profile output hash differs from source hash".into());
    }
    if profile == Profile::Av1
        && !output
            .format
            .format_name
            .as_deref()
            .is_some_and(|names| names.split(',').any(|name| name == "matroska"))
    {
        return Err("AV1 output is not Matroska".into());
    }
    if source.streams.len() != output.streams.len() {
        return Err("stream count changed".into());
    }
    for (source_stream, output_stream) in source.streams.iter().zip(&output.streams) {
        if source_stream.codec_type != output_stream.codec_type {
            return Err(format!("stream {} type changed", source_stream.index));
        }
        if source_stream.width != output_stream.width
            || source_stream.height != output_stream.height
        {
            return Err(format!("stream {} dimensions changed", source_stream.index));
        }
        compare_duration(
            source_stream.duration_seconds,
            output_stream.duration_seconds,
            tolerance,
            &format!("stream {}", source_stream.index),
        )?;
        let transcoded = profile == Profile::Av1
            && source_stream.codec_type.as_deref() == Some("video")
            && !source_stream.attached_picture;
        if transcoded {
            if output_stream.codec_name.as_deref() != Some("av1") {
                return Err(format!("stream {} is not AV1", source_stream.index));
            }
            compare_reported(
                &source_stream.pixel_format,
                &output_stream.pixel_format,
                "pixel format",
                source_stream.index,
            )?;
            if source_stream.pixel_format.is_none() {
                compare_reported(
                    &source_stream.bits_per_raw_sample,
                    &output_stream.bits_per_raw_sample,
                    "bit depth",
                    source_stream.index,
                )?;
            }
            compare_reported(
                &source_stream.color_space,
                &output_stream.color_space,
                "color space",
                source_stream.index,
            )?;
            compare_reported(
                &source_stream.color_transfer,
                &output_stream.color_transfer,
                "color transfer",
                source_stream.index,
            )?;
            compare_reported(
                &source_stream.color_primaries,
                &output_stream.color_primaries,
                "color primaries",
                source_stream.index,
            )?;
        } else if source_stream.codec_name != output_stream.codec_name {
            return Err(format!(
                "preserved stream {} codec changed",
                source_stream.index
            ));
        }
    }
    compare_duration(
        source.format.duration_seconds,
        output.format.duration_seconds,
        tolerance,
        "container",
    )?;
    if source.chapters.len() != output.chapters.len() {
        return Err("chapter count changed".into());
    }
    for (index, (source_chapter, output_chapter)) in
        source.chapters.iter().zip(&output.chapters).enumerate()
    {
        if (source_chapter.start_seconds - output_chapter.start_seconds).abs() > tolerance
            || (source_chapter.end_seconds - output_chapter.end_seconds).abs() > tolerance
        {
            return Err(format!("chapter {index} timing changed"));
        }
    }
    Ok(())
}

fn compare_duration(
    source: Option<f64>,
    output: Option<f64>,
    tolerance: f64,
    subject: &str,
) -> Result<()> {
    match (source, output) {
        (Some(source), Some(output)) if (source - output).abs() <= tolerance => Ok(()),
        (None, None) => Ok(()),
        _ => Err(format!("{subject} duration changed beyond tolerance")),
    }
}

fn compare_reported(
    source: &Option<String>,
    output: &Option<String>,
    field: &str,
    index: u32,
) -> Result<()> {
    if source.is_some() && source != output {
        Err(format!("stream {index} {field} changed"))
    } else {
        Ok(())
    }
}

fn decode_output(path: &Path) -> Result<()> {
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0", "-f", "null", "-"])
        .output()
        .map_err(|error| format!("failed to execute ffmpeg: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(command_failure("ffmpeg full decode", &output.stderr))
    }
}

fn restore(manifest: &ArchiveManifest) -> Result<()> {
    if manifest.source_disposition != SourceDisposition::Quarantine {
        return Err("restore is only valid for quarantined sources".into());
    }
    let source = Path::new(&manifest.source.path);
    let quarantine = Path::new(
        manifest
            .quarantine_path
            .as_deref()
            .ok_or_else(|| "manifest has no quarantinePath".to_string())?,
    );
    if quarantine.exists() {
        let quarantine = regular_file(quarantine)?;
        if hash_file(&quarantine)? != manifest.source.sha256 {
            return Err("quarantined source SHA-256 differs from manifest".into());
        }
    }
    match (source.exists(), quarantine.exists()) {
        (false, true) => {
            fs::hard_link(quarantine, source).map_err(|error| {
                format!("restore hard-link failed; no copy fallback is permitted: {error}")
            })?;
            File::open(source)
                .and_then(|file| file.sync_all())
                .map_err(|error| format!("cannot sync restored source: {error}"))?;
            sync_parent(source)?;
            fs::remove_file(quarantine)
                .map_err(|error| format!("cannot unlink quarantine after restore: {error}"))?;
            sync_parent(quarantine)
        }
        (true, true) => {
            ensure_same_file(source, quarantine)?;
            fs::remove_file(quarantine)
                .map_err(|error| format!("cannot finish interrupted restore: {error}"))?;
            sync_parent(quarantine)
        }
        (true, false) => {
            let source = regular_file(source)?;
            if hash_file(&source)? == manifest.source.sha256 {
                Ok(())
            } else {
                Err("existing source does not match manifest; refusing overwrite".into())
            }
        }
        (false, false) => Err("both source and quarantine are missing".into()),
    }
}

#[cfg(unix)]
fn ensure_same_file(left: &Path, right: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let left_meta =
        fs::metadata(left).map_err(|error| format!("cannot stat '{}': {error}", left.display()))?;
    let right_meta = fs::metadata(right)
        .map_err(|error| format!("cannot stat '{}': {error}", right.display()))?;
    if left_meta.dev() == right_meta.dev() && left_meta.ino() == right_meta.ino() {
        Ok(())
    } else {
        Err("existing files are not links to the same inode".into())
    }
}

#[cfg(not(unix))]
fn ensure_same_file(left: &Path, right: &Path) -> Result<()> {
    if hash_file(left)? == hash_file(right)? {
        Ok(())
    } else {
        Err("existing files differ".into())
    }
}

fn ensure_directory(path: &Path) -> Result<()> {
    match fs::create_dir(path) {
        Ok(()) => sync_parent(path),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path).map_err(|error| {
                format!("cannot inspect directory '{}': {error}", path.display())
            })?;
            if metadata.file_type().is_dir() {
                Ok(())
            } else {
                Err(format!("refusing non-directory path: {}", path.display()))
            }
        }
        Err(error) => Err(format!(
            "cannot create directory '{}': {error}",
            path.display()
        )),
    }
}

fn remove_internal_regular(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect internal file '{}': {error}", path.display()))?;
    if !metadata.file_type().is_file() {
        return Err(format!(
            "refusing to remove non-regular internal path: {}",
            path.display()
        ));
    }
    fs::remove_file(path)
        .map_err(|error| format!("cannot remove internal file '{}': {error}", path.display()))
}

fn atomic_replace(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| "output file name must be UTF-8".to_string())?;
    let temporary = parent.join(format!(".{name}.marchiver.tmp"));
    if temporary.exists() {
        remove_internal_regular(&temporary)?;
    }
    write_new_synced(&temporary, contents)?;
    fs::rename(&temporary, path)
        .map_err(|error| format!("cannot publish '{}': {error}", path.display()))?;
    sync_directory(parent)
}

fn write_user_output(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| "output file name must be UTF-8".to_string())?;
    let temporary = parent.join(format!(".{name}.marchiver-{}.tmp", std::process::id()));
    write_new_synced(&temporary, contents)?;
    if let Err(error) = fs::hard_link(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(format!(
            "refusing to replace output '{}': {error}",
            path.display()
        ));
    }
    sync_parent(path)?;
    fs::remove_file(&temporary)
        .map_err(|error| format!("cannot remove output staging file: {error}"))?;
    sync_parent(&temporary)
}

fn publish_text_no_replace(temporary: &Path, destination: &Path, contents: &[u8]) -> Result<()> {
    if temporary.exists() {
        remove_internal_regular(temporary)?;
    }
    write_new_synced(temporary, contents)?;
    if let Err(error) = fs::hard_link(temporary, destination) {
        let _ = fs::remove_file(temporary);
        return Err(format!(
            "no-replace publication '{}' failed: {error}",
            destination.display()
        ));
    }
    sync_parent(destination)?;
    fs::remove_file(temporary)
        .map_err(|error| format!("cannot remove manifest staging: {error}"))?;
    sync_parent(temporary)
}

fn write_new_synced(path: &Path, contents: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("cannot create '{}': {error}", path.display()))?;
    file.write_all(contents)
        .map_err(|error| format!("cannot write '{}': {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("cannot sync '{}': {error}", path.display()))
}

fn write_journal(path: &Path, journal: &Journal) -> Result<()> {
    atomic_replace(path, render_journal(journal).as_bytes())
}

fn sync_parent(path: &Path) -> Result<()> {
    sync_directory(path.parent().unwrap_or_else(|| Path::new(".")))
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("cannot sync directory '{}': {error}", path.display()))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

fn path_string(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("path is not valid UTF-8: {}", path.display()))
}

fn render_inspection(inspection: &ArchiveInspection) -> String {
    let mut out = document_header(
        inspection.schema_version,
        &inspection.kind,
        &inspection.producer_version,
    );
    out.push_str("evidence = ");
    render_evidence(&mut out, &inspection.evidence, 0);
    out.push('\n');
    out
}

fn render_plan(plan: &ArchivePlan) -> String {
    let mut out = document_header(plan.schema_version, &plan.kind, &plan.producer_version);
    render_common_plan_fields(
        &mut out,
        &plan.transaction_id,
        plan.profile,
        plan.source_disposition,
        plan.duration_tolerance_seconds,
        &plan.av1,
        &plan.source,
        &plan.destination_path,
        &plan.manifest_path,
    );
    out
}

fn render_manifest(manifest: &ArchiveManifest) -> String {
    let mut out = document_header(
        manifest.schema_version,
        &manifest.kind,
        &manifest.producer_version,
    );
    render_common_plan_fields(
        &mut out,
        &manifest.transaction_id,
        manifest.profile,
        manifest.source_disposition,
        manifest.duration_tolerance_seconds,
        &manifest.av1,
        &manifest.source,
        &manifest.destination_path,
        &manifest.manifest_path,
    );
    out.push_str("outputEvidence = ");
    render_evidence(&mut out, &manifest.output, 0);
    out.push('\n');
    render_optional_string(&mut out, "quarantinePath", &manifest.quarantine_path);
    out
}

#[allow(clippy::too_many_arguments)]
fn render_common_plan_fields(
    out: &mut String,
    transaction_id: &str,
    profile: Profile,
    disposition: SourceDisposition,
    tolerance: f64,
    av1: &Av1Config,
    source: &MediaEvidence,
    destination: &str,
    manifest: &str,
) {
    render_string(out, "transactionId", transaction_id);
    render_string(out, "profile", profile.as_str());
    render_string(out, "sourceDisposition", disposition.as_str());
    out.push_str(&format!("durationToleranceSeconds = {tolerance:?}\n"));
    out.push_str("av1 = new {\n");
    out.push_str(&format!("  crf = {}\n", av1.crf));
    out.push_str(&format!("  preset = {}\n", av1.preset));
    out.push_str(&format!(
        "  container = {}\n",
        pklx::pkl_string_literal(&av1.container)
    ));
    out.push_str("}\n");
    out.push_str("source = ");
    render_evidence(out, source, 0);
    out.push('\n');
    render_string(out, "destinationPath", destination);
    render_string(out, "manifestPath", manifest);
}

fn render_journal(journal: &Journal) -> String {
    let mut out = document_header(
        journal.schema_version,
        &journal.kind,
        &journal.producer_version,
    );
    render_string(&mut out, "transactionId", &journal.transaction_id);
    render_string(&mut out, "sourcePath", &journal.source_path);
    render_string(&mut out, "destinationPath", &journal.destination_path);
    render_string(&mut out, "manifestPath", &journal.manifest_path);
    render_string(&mut out, "stagingPath", &journal.staging_path);
    render_string(&mut out, "phase", journal.phase.as_str());
    if let Some(output) = &journal.output {
        out.push_str("outputEvidence = ");
        render_evidence(&mut out, output, 0);
        out.push('\n');
    } else {
        out.push_str("outputEvidence = null\n");
    }
    render_optional_string(&mut out, "quarantinePath", &journal.quarantine_path);
    render_optional_string(&mut out, "diagnostic", &journal.diagnostic);
    out
}

fn document_header(schema: i64, kind: &str, producer: &str) -> String {
    format!(
        "// Trusted local Marchiver state. Do not evaluate untrusted Pkl.\nschemaVersion = {schema}\nkind = {}\nproducerVersion = {}\n",
        pklx::pkl_string_literal(kind),
        pklx::pkl_string_literal(producer)
    )
}

fn render_evidence(out: &mut String, evidence: &MediaEvidence, indent: usize) {
    let pad = " ".repeat(indent);
    let field = " ".repeat(indent + 2);
    out.push_str("new {\n");
    render_indented_string(out, &field, "path", &evidence.path);
    render_indented_string(out, &field, "sha256", &evidence.sha256);
    out.push_str(&format!("{field}size = {}\n", evidence.size));
    out.push_str(&format!("{field}modifiedNs = {}\n", evidence.modified_ns));
    out.push_str(&format!("{field}format = new {{\n"));
    render_indented_optional_string(
        out,
        &(field.clone() + "  "),
        "formatName",
        &evidence.format.format_name,
    );
    render_indented_optional_float(
        out,
        &(field.clone() + "  "),
        "durationSeconds",
        evidence.format.duration_seconds,
    );
    out.push_str(&format!("{field}}}\n"));
    out.push_str(&format!("{field}streams = List(\n"));
    for stream in &evidence.streams {
        out.push_str(&format!("{field}  new {{\n"));
        let item = field.clone() + "    ";
        out.push_str(&format!("{item}index = {}\n", stream.index));
        render_indented_optional_string(out, &item, "codecName", &stream.codec_name);
        render_indented_optional_string(out, &item, "codecType", &stream.codec_type);
        render_indented_optional_u32(out, &item, "width", stream.width);
        render_indented_optional_u32(out, &item, "height", stream.height);
        render_indented_optional_float(out, &item, "durationSeconds", stream.duration_seconds);
        render_indented_optional_string(
            out,
            &item,
            "bitsPerRawSample",
            &stream.bits_per_raw_sample,
        );
        render_indented_optional_string(out, &item, "pixelFormat", &stream.pixel_format);
        render_indented_optional_string(out, &item, "colorSpace", &stream.color_space);
        render_indented_optional_string(out, &item, "colorTransfer", &stream.color_transfer);
        render_indented_optional_string(out, &item, "colorPrimaries", &stream.color_primaries);
        out.push_str(&format!(
            "{item}attachedPicture = {}\n",
            stream.attached_picture
        ));
        out.push_str(&format!("{field}  }},\n"));
    }
    out.push_str(&format!("{field})\n"));
    out.push_str(&format!("{field}chapters = List(\n"));
    for chapter in &evidence.chapters {
        out.push_str(&format!(
            "{field}  new {{ startSeconds = {:?}; endSeconds = {:?} }},\n",
            chapter.start_seconds, chapter.end_seconds
        ));
    }
    out.push_str(&format!("{field})\n"));
    render_indented_string(out, &field, "rawProbeJson", &evidence.raw_probe_json);
    out.push_str(&format!("{pad}}}"));
}

fn render_string(out: &mut String, name: &str, value: &str) {
    out.push_str(&format!("{name} = {}\n", pklx::pkl_string_literal(value)));
}

fn render_optional_string(out: &mut String, name: &str, value: &Option<String>) {
    match value {
        Some(value) => render_string(out, name, value),
        None => out.push_str(&format!("{name} = null\n")),
    }
}

fn render_indented_string(out: &mut String, indent: &str, name: &str, value: &str) {
    out.push_str(&format!(
        "{indent}{name} = {}\n",
        pklx::pkl_string_literal(value)
    ));
}

fn render_indented_optional_string(
    out: &mut String,
    indent: &str,
    name: &str,
    value: &Option<String>,
) {
    match value {
        Some(value) => render_indented_string(out, indent, name, value),
        None => out.push_str(&format!("{indent}{name} = null\n")),
    }
}

fn render_indented_optional_float(out: &mut String, indent: &str, name: &str, value: Option<f64>) {
    match value {
        Some(value) => out.push_str(&format!("{indent}{name} = {value:?}\n")),
        None => out.push_str(&format!("{indent}{name} = null\n")),
    }
}

fn render_indented_optional_u32(out: &mut String, indent: &str, name: &str, value: Option<u32>) {
    match value {
        Some(value) => out.push_str(&format!("{indent}{name} = {value}\n")),
        None => out.push_str(&format!("{indent}{name} = null\n")),
    }
}
