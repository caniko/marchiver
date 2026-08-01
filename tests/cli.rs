use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_marchiver"))
        .args(args)
        .output()
        .expect("run marchiver")
}

#[test]
fn default_and_help_flags_print_usage() {
    for args in [Vec::<&str>::new(), vec!["--help"], vec!["-h"]] {
        let output = run(&args);
        assert!(output.status.success(), "args {args:?}: {output:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.starts_with("marchiver "), "args {args:?}: {stdout}");
        assert!(stdout.contains("Usage:\n  marchiver [OPTIONS]"));
        assert!(stdout.contains("marchiver <COMMAND>"));
        assert!(output.stderr.is_empty(), "args {args:?}: {output:?}");
    }
}

#[test]
fn version_flags_print_package_version() {
    for args in [vec!["--version"], vec!["-V"]] {
        let output = run(&args);
        assert!(output.status.success(), "args {args:?}: {output:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            env!("CARGO_PKG_VERSION")
        );
        assert!(output.stderr.is_empty(), "args {args:?}: {output:?}");
    }
}

#[test]
fn unknown_or_extra_arguments_fail_with_usage_hint() {
    for args in [vec!["--unknown"], vec!["--help", "extra"]] {
        let output = run(&args);
        assert_eq!(output.status.code(), Some(2), "args {args:?}: {output:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("marchiver --help"));
        assert!(output.stdout.is_empty(), "args {args:?}: {output:?}");
    }
}

#[test]
fn subcommand_help_is_available() {
    for command in ["inspect", "plan", "apply", "verify", "restore"] {
        let output = run(&[command, "--help"]);
        assert!(output.status.success(), "{command}: {output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains(&format!("marchiver {command}")));
        assert!(output.stderr.is_empty(), "{command}: {output:?}");
    }
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let unique = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "marchiver-{label}-{}-{nanos}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create temp directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
struct FakeTools {
    _temp: TempDir,
    root: PathBuf,
    bin: PathBuf,
    log: PathBuf,
    json: PathBuf,
    av1_json: PathBuf,
}

#[cfg(unix)]
impl FakeTools {
    fn new(label: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new(label);
        let root = temp.path().join("archive paths with spaces");
        let bin = temp.path().join("fake tools");
        fs::create_dir(&root).expect("create archive root");
        fs::create_dir(&bin).expect("create fake tool directory");
        let log = temp.path().join("argv.log");
        let json = temp.path().join("probe.json");
        let av1_json = temp.path().join("probe-av1.json");
        fs::write(
            &json,
            r#"{
  "streams": [{
    "index": 0,
    "codec_name": "h264",
    "codec_type": "video",
    "width": 1920,
    "height": 1080,
    "duration": "1.000000",
    "bits_per_raw_sample": "8",
    "pix_fmt": "yuv420p",
    "color_space": "bt709",
    "color_transfer": "bt709",
    "color_primaries": "bt709",
    "disposition": {"attached_pic": 0}
  }],
  "chapters": [{"start_time": "0.000000", "end_time": "1.000000"}],
  "format": {"format_name": "matroska", "duration": "1.000000"}
}"#,
        )
        .expect("write probe JSON");
        fs::write(
            &av1_json,
            fs::read_to_string(&json)
                .expect("read probe JSON")
                .replace("\"h264\"", "\"av1\""),
        )
        .expect("write AV1 probe JSON");
        let ffprobe = bin.join("ffprobe");
        fs::write(
            &ffprobe,
            r#"#!/bin/sh
set -eu
{
  printf '%s\n' 'BEGIN ffprobe'
  printf '%s\n' "$@"
  printf '%s\n' 'END'
} >> "$MARCHIVER_TEST_LOG"
if [ "${MARCHIVER_TEST_PROBE_FAIL:-}" = 1 ]; then
  printf '%s\n' 'probe exploded safely' >&2
  exit 19
fi
[ "$#" -eq 8 ]
[ "$1" = -v ]
[ "$2" = error ]
[ "$3" = -show_format ]
[ "$4" = -show_streams ]
[ "$5" = -show_chapters ]
[ "$6" = -of ]
[ "$7" = json ]
if [ -n "${MARCHIVER_TEST_AV1_SOURCE:-}" ] && [ "$8" != "$MARCHIVER_TEST_AV1_SOURCE" ]; then
  cat "$MARCHIVER_TEST_AV1_JSON"
else
  cat "$MARCHIVER_TEST_JSON"
fi
"#,
        )
        .expect("write fake ffprobe");
        fs::set_permissions(&ffprobe, fs::Permissions::from_mode(0o755))
            .expect("make ffprobe executable");

        let ffmpeg = bin.join("ffmpeg");
        fs::write(
            &ffmpeg,
            r#"#!/bin/sh
set -eu
{
  printf '%s\n' 'BEGIN ffmpeg'
  printf '%s\n' "$@"
  printf '%s\n' 'END'
} >> "$MARCHIVER_TEST_LOG"
if [ "${MARCHIVER_TEST_FFMPEG_FAIL:-}" = 1 ]; then
  printf '%s\n' 'encode exploded safely' >&2
  exit 23
fi
if [ "$#" -ge 6 ] && [ "$3" = -nostdin ]; then
  for last do :; done
  cp "$6" "$last"
else
  [ "$#" -eq 9 ]
  [ "$1" = -v ]
  [ "$2" = error ]
  [ "$3" = -i ]
  [ "$5" = -map ]
  [ "$6" = 0 ]
  [ "$7" = -f ]
  [ "$8" = null ]
  [ "$9" = - ]
fi
"#,
        )
        .expect("write fake ffmpeg");
        fs::set_permissions(&ffmpeg, fs::Permissions::from_mode(0o755))
            .expect("make ffmpeg executable");

        Self {
            _temp: temp,
            root,
            bin,
            log,
            json,
            av1_json,
        }
    }

    fn command<I, S>(&self, args: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = Command::new(env!("CARGO_BIN_EXE_marchiver"));
        command.args(args);
        let mut path = OsString::from(&self.bin);
        path.push(":");
        path.push(std::env::var_os("PATH").unwrap_or_default());
        command
            .env("PATH", path)
            .env("MARCHIVER_TEST_LOG", &self.log)
            .env("MARCHIVER_TEST_JSON", &self.json)
            .env("MARCHIVER_TEST_AV1_JSON", &self.av1_json);
        command
    }

    fn run<I, S>(&self, args: I) -> Output
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.command(args).output().expect("run marchiver")
    }

    fn source(&self, name: &str) -> PathBuf {
        let path = self.root.join(name);
        fs::write(&path, b"one media file\n").expect("write source");
        path
    }

    fn config(&self, disposition: &str) -> PathBuf {
        let path = self.root.join(format!("{disposition} config.pkl"));
        fs::write(
            &path,
            format!(
                "schemaVersion = 1\nsourceDisposition = \"{disposition}\"\ndefaultProfile = \"copy\"\nav1 = new {{ crf = 20; preset = 6; container = \"mkv\" }}\ndurationToleranceSeconds = 0.5\n"
            ),
        )
        .expect("write config");
        path
    }

    fn plan(
        &self,
        source: &Path,
        destination: &Path,
        disposition: Option<&str>,
        profile: Option<&str>,
    ) -> PathBuf {
        let plan = self.root.join(format!(
            "{} plan.pkl",
            destination.file_name().unwrap().to_string_lossy()
        ));
        let mut args = vec![
            OsString::from("plan"),
            source.as_os_str().to_owned(),
            destination.as_os_str().to_owned(),
        ];
        if let Some(profile) = profile {
            args.extend([OsString::from("--profile"), OsString::from(profile)]);
        }
        if let Some(disposition) = disposition {
            args.extend([
                OsString::from("--config"),
                self.config(disposition).into_os_string(),
            ]);
        }
        args.extend([OsString::from("--output"), plan.as_os_str().to_owned()]);
        let output = self.run(args);
        assert_success(&output);
        plan
    }
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
fn only_entry(path: &Path) -> PathBuf {
    let entries: Vec<_> = fs::read_dir(path)
        .expect("read directory")
        .map(|entry| entry.expect("read entry").path())
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "entries in {}: {entries:?}",
        path.display()
    );
    entries[0].clone()
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn inspect_uses_exact_argv_without_shell_and_renders_roundtrip_pkl() {
    let fixture = FakeTools::new("inspect");
    let source = fixture.source("source ; touch SHELL_WAS_USED.media");
    let inspection = fixture.root.join("inspection evidence.pkl");
    let output = fixture.run([
        OsStr::new("inspect"),
        source.as_os_str(),
        OsStr::new("--output"),
        inspection.as_os_str(),
    ]);
    assert_success(&output);
    assert!(!fixture.root.join("SHELL_WAS_USED.media").exists());

    let log = fs::read_to_string(&fixture.log).expect("read argv log");
    let expected = format!(
        "BEGIN ffprobe\n-v\nerror\n-show_format\n-show_streams\n-show_chapters\n-of\njson\n{}\nEND\n",
        source.display()
    );
    assert_eq!(log, expected);
    let value = pklx::eval_to_value(&inspection, pklx::pklr::EvalOptions::default())
        .await
        .expect("round-trip inspection through pklx");
    let rendered = format!("{value:?}");
    assert!(rendered.contains("ArchiveInspection"));
    assert!(rendered.contains("rawProbeJson"));
}

#[cfg(unix)]
#[test]
fn tool_stderr_is_reported_and_no_output_is_published() {
    let fixture = FakeTools::new("probe-failure");
    let source = fixture.source("bad source.media");
    let inspection = fixture.root.join("should not exist.pkl");
    let output = fixture
        .command([
            OsStr::new("inspect"),
            source.as_os_str(),
            OsStr::new("--output"),
            inspection.as_os_str(),
        ])
        .env("MARCHIVER_TEST_PROBE_FAIL", "1")
        .output()
        .expect("run failing inspect");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("probe exploded safely"));
    assert!(!inspection.exists());
}

#[cfg(unix)]
#[test]
fn inspect_and_plan_refuse_to_replace_existing_files() {
    let fixture = FakeTools::new("output-no-replace");
    let source = fixture.source("source.media");
    let original = fs::read(&source).unwrap();

    let inspect = fixture.run([
        OsStr::new("inspect"),
        source.as_os_str(),
        OsStr::new("--output"),
        source.as_os_str(),
    ]);
    assert!(!inspect.status.success());
    assert_eq!(fs::read(&source).unwrap(), original);

    let occupied_plan = fixture.root.join("occupied plan.pkl");
    fs::write(&occupied_plan, b"keep this plan\n").unwrap();
    let destination = fixture.root.join("destination.media");
    let plan = fixture.run([
        OsStr::new("plan"),
        source.as_os_str(),
        destination.as_os_str(),
        OsStr::new("--output"),
        occupied_plan.as_os_str(),
    ]);
    assert!(!plan.status.success());
    assert_eq!(fs::read(&occupied_plan).unwrap(), b"keep this plan\n");
    assert_eq!(fs::read(&source).unwrap(), original);
}

#[cfg(unix)]
#[test]
fn copy_quarantine_verify_restore_and_complete_resume() {
    let fixture = FakeTools::new("quarantine");
    let source = fixture.source("source movie.media");
    let destination = fixture.root.join("archived movie.media");
    let plan = fixture.plan(&source, &destination, None, None);

    let output = fixture.run([OsStr::new("apply"), plan.as_os_str()]);
    assert_success(&output);
    assert!(!source.exists());
    assert_eq!(fs::read(&destination).unwrap(), b"one media file\n");
    let manifest = fixture.root.join("archived movie.media.marchiver.pkl");
    assert!(manifest.exists());
    let quarantine_transaction = only_entry(&fixture.root.join(".marchiver-quarantine"));
    let quarantine = quarantine_transaction.join("source movie.media");
    assert!(quarantine.exists());

    assert_success(&fixture.run([OsStr::new("verify"), manifest.as_os_str()]));
    let already_complete = fixture.run([OsStr::new("apply"), plan.as_os_str()]);
    assert_success(&already_complete);
    assert!(String::from_utf8_lossy(&already_complete.stdout).contains("already complete"));

    fs::write(&source, b"foreign existing source").unwrap();
    let refused = fixture.run([OsStr::new("restore"), manifest.as_os_str()]);
    assert!(!refused.status.success());
    assert_eq!(fs::read(&source).unwrap(), b"foreign existing source");
    fs::remove_file(&source).unwrap();
    assert_success(&fixture.run([OsStr::new("restore"), manifest.as_os_str()]));
    assert!(source.exists());
    assert!(!quarantine.exists());
    assert_success(&fixture.run([OsStr::new("restore"), manifest.as_os_str()]));
}

#[cfg(unix)]
#[test]
fn preserve_and_delete_dispositions_are_applied_after_manifest() {
    for disposition in ["preserve", "delete"] {
        let fixture = FakeTools::new(disposition);
        let source = fixture.source(&format!("{disposition} source.media"));
        let destination = fixture.root.join(format!("{disposition} output.media"));
        let plan = fixture.plan(&source, &destination, Some(disposition), None);
        assert_success(&fixture.run([OsStr::new("apply"), plan.as_os_str()]));
        assert!(destination.exists());
        assert!(
            fixture
                .root
                .join(format!("{disposition} output.media.marchiver.pkl"))
                .exists()
        );
        assert_eq!(source.exists(), disposition == "preserve");
        assert!(!fixture.root.join(".marchiver-quarantine").exists());
    }
}

#[cfg(unix)]
#[test]
fn existing_destination_and_changed_source_are_refused() {
    let fixture = FakeTools::new("refusals");
    let source = fixture.source("source.media");
    let occupied = fixture.root.join("occupied.media");
    let occupied_plan = fixture.plan(&source, &occupied, Some("preserve"), None);
    fs::write(&occupied, b"do not overwrite").unwrap();
    let output = fixture.run([OsStr::new("apply"), occupied_plan.as_os_str()]);
    assert!(!output.status.success());
    assert_eq!(fs::read(&occupied).unwrap(), b"do not overwrite");

    let changed_destination = fixture.root.join("changed output.media");
    let changed_plan = fixture.plan(&source, &changed_destination, Some("preserve"), None);
    fs::write(&source, b"mutated content\n").unwrap();
    let output = fixture.run([OsStr::new("apply"), changed_plan.as_os_str()]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("source changed"));
    assert!(!changed_destination.exists());
}

#[cfg(unix)]
#[test]
fn duplicate_quarantine_link_phase_resumes_without_losing_either_copy() {
    let fixture = FakeTools::new("quarantine-resume");
    let source = fixture.source("resume source.media");
    let destination = fixture.root.join("resume output.media");
    let plan = fixture.plan(&source, &destination, None, None);
    assert_success(&fixture.run([OsStr::new("apply"), plan.as_os_str()]));

    let quarantine_transaction = only_entry(&fixture.root.join(".marchiver-quarantine"));
    let quarantine = quarantine_transaction.join("resume source.media");
    fs::hard_link(&quarantine, &source).expect("simulate duplicate-link crash phase");
    let transaction = only_entry(&fixture.root.join(".marchiver-transactions"));
    let journal = transaction.join("journal.pkl");
    let contents = fs::read_to_string(&journal).unwrap();
    assert!(contents.contains("phase = \"complete\""));
    fs::write(
        &journal,
        contents.replace("phase = \"complete\"", "phase = \"quarantineLinked\""),
    )
    .unwrap();

    assert_success(&fixture.run([OsStr::new("apply"), plan.as_os_str()]));
    assert!(!source.exists());
    assert!(quarantine.exists());
}

#[cfg(unix)]
#[test]
fn schema_versions_are_rejected_before_typed_deserialization() {
    let fixture = FakeTools::new("schema");
    let source = fixture.source("schema source.media");
    let destination = fixture.root.join("schema output.media");
    let bad_config = fixture.root.join("bad config.pkl");
    fs::write(
        &bad_config,
        "schemaVersion = 2\nsourceDisposition = \"preserve\"\ndefaultProfile = \"copy\"\nav1 = new { crf = 20; preset = 6; container = \"mkv\" }\ndurationToleranceSeconds = 0.5\n",
    )
    .unwrap();
    let plan = fixture.root.join("bad plan.pkl");
    let output = fixture.run([
        OsStr::new("plan"),
        source.as_os_str(),
        destination.as_os_str(),
        OsStr::new("--config"),
        bad_config.as_os_str(),
        OsStr::new("--output"),
        plan.as_os_str(),
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("schemaVersion 2"));
    assert!(!plan.exists());
}

#[cfg(unix)]
#[test]
fn av1_argv_is_explicit_and_failure_is_journaled_safely() {
    let fixture = FakeTools::new("av1-argv");
    let source = fixture.source("AV1 source with spaces.media");
    let destination = fixture.root.join("AV1 destination with spaces.mkv");
    let plan = fixture.plan(&source, &destination, Some("preserve"), Some("av1"));
    let output = fixture
        .command([OsStr::new("apply"), plan.as_os_str()])
        .env("MARCHIVER_TEST_FFMPEG_FAIL", "1")
        .output()
        .expect("run failed AV1 apply");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("encode exploded safely"));
    assert!(source.exists());
    assert!(!destination.exists());

    let log = fs::read_to_string(&fixture.log).unwrap();
    let transaction = only_entry(&fixture.root.join(".marchiver-transactions"));
    let staging = transaction.join("output.partial");
    let expected = format!(
        "BEGIN ffmpeg\n-v\nerror\n-nostdin\n-n\n-i\n{}\n-map\n0\n-map_metadata\n0\n-map_chapters\n0\n-c\ncopy\n-c:v:0\nlibsvtav1\n-crf:v:0\n20\n-preset:v:0\n6\n-pix_fmt:v:0\nyuv420p\n-color_primaries:v:0\nbt709\n-color_trc:v:0\nbt709\n-colorspace:v:0\nbt709\n-f\nmatroska\n{}\nEND\n",
        source.display(),
        staging.display()
    );
    assert!(
        log.ends_with(&expected),
        "log:\n{log}\nexpected suffix:\n{expected}"
    );
    let journal = fs::read_to_string(transaction.join("journal.pkl")).unwrap();
    assert!(journal.contains("phase = \"prepared\""));
    assert!(journal.contains("encode exploded safely"));
}

#[cfg(unix)]
#[test]
fn av1_apply_and_verify_enforce_transcoded_stream_semantics() {
    let fixture = FakeTools::new("av1-success");
    let source = fixture.source("semantic AV1 source.media");
    let destination = fixture.root.join("semantic AV1 output.mkv");
    let plan = fixture.plan(&source, &destination, Some("preserve"), Some("av1"));
    let output = fixture
        .command([OsStr::new("apply"), plan.as_os_str()])
        .env("MARCHIVER_TEST_AV1_SOURCE", &source)
        .output()
        .expect("apply fake AV1 plan");
    assert_success(&output);
    assert!(source.exists());
    assert!(destination.exists());
    let manifest = fixture.root.join("semantic AV1 output.mkv.marchiver.pkl");
    let output = fixture
        .command([OsStr::new("verify"), manifest.as_os_str()])
        .env("MARCHIVER_TEST_AV1_SOURCE", &source)
        .output()
        .expect("verify fake AV1 output");
    assert_success(&output);
}

#[cfg(unix)]
#[test]
fn real_ffmpeg_tiny_media_copy_and_optional_av1() {
    if Command::new("ffmpeg").arg("-version").output().is_err()
        || Command::new("ffprobe").arg("-version").output().is_err()
    {
        eprintln!("skipping real-media test: ffmpeg or ffprobe is unavailable");
        return;
    }
    let temp = TempDir::new("real-ffmpeg");
    let source = temp.path().join("tiny source.mkv");
    let generated = Command::new("ffmpeg")
        .args(["-v", "error", "-f", "lavfi", "-i"])
        .arg("testsrc2=size=64x64:rate=5:duration=0.4")
        .args(["-f", "lavfi", "-i"])
        .arg("sine=frequency=1000:duration=0.4")
        .args(["-c:v", "ffv1", "-c:a", "pcm_s16le", "-shortest"])
        .arg(&source)
        .output()
        .expect("run real ffmpeg generator");
    if !generated.status.success() {
        eprintln!(
            "skipping real-media test: fixture generation failed: {}",
            String::from_utf8_lossy(&generated.stderr)
        );
        return;
    }
    let config = temp.path().join("preserve.pkl");
    fs::write(
        &config,
        "schemaVersion = 1\nsourceDisposition = \"preserve\"\ndefaultProfile = \"copy\"\nav1 = new { crf = 20; preset = 6; container = \"mkv\" }\ndurationToleranceSeconds = 0.5\n",
    )
    .unwrap();

    let copy = temp.path().join("tiny copy.mkv");
    let copy_plan = temp.path().join("copy plan.pkl");
    assert_success(
        &Command::new(env!("CARGO_BIN_EXE_marchiver"))
            .args(["plan"])
            .arg(&source)
            .arg(&copy)
            .args(["--config"])
            .arg(&config)
            .args(["--output"])
            .arg(&copy_plan)
            .output()
            .unwrap(),
    );
    assert_success(
        &Command::new(env!("CARGO_BIN_EXE_marchiver"))
            .arg("apply")
            .arg(&copy_plan)
            .output()
            .unwrap(),
    );
    let copy_manifest = temp.path().join("tiny copy.mkv.marchiver.pkl");
    assert_success(
        &Command::new(env!("CARGO_BIN_EXE_marchiver"))
            .arg("verify")
            .arg(&copy_manifest)
            .output()
            .unwrap(),
    );

    let encoders = Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .expect("list ffmpeg encoders");
    if !String::from_utf8_lossy(&encoders.stdout).contains("libsvtav1") {
        eprintln!("skipping real AV1 subtest: ffmpeg has no libsvtav1 encoder");
        return;
    }
    let av1 = temp.path().join("tiny AV1.mkv");
    let av1_plan = temp.path().join("AV1 plan.pkl");
    assert_success(
        &Command::new(env!("CARGO_BIN_EXE_marchiver"))
            .args(["plan"])
            .arg(&source)
            .arg(&av1)
            .args(["--profile", "av1", "--config"])
            .arg(&config)
            .args(["--output"])
            .arg(&av1_plan)
            .output()
            .unwrap(),
    );
    assert_success(
        &Command::new(env!("CARGO_BIN_EXE_marchiver"))
            .arg("apply")
            .arg(&av1_plan)
            .output()
            .unwrap(),
    );
    assert_success(
        &Command::new(env!("CARGO_BIN_EXE_marchiver"))
            .arg("verify")
            .arg(temp.path().join("tiny AV1.mkv.marchiver.pkl"))
            .output()
            .unwrap(),
    );
}
