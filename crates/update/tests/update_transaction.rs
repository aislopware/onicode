//! An update's failure paths, with real processes: this test binary, copied
//! into a real macOS app layout (the installed `Onicode.app` and the new bundle
//! beside it), plays both the old and the new engine. It really replaces itself
//! with the new binary, really gets killed by the watchdog, and really crashes
//! (`abort`) at each step. The test plays the service manager, starting
//! `Onicode.app/Contents/MacOS/onicode` until the update has settled. The
//! installed bundle only changes at the commit, in one atomic swap.
//!
//! The databases here are plain files, since only their bytes matter to the backup.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use onicode_update::transaction::{self, BootOptions, Layout, Phase, RestartMode};

const DATA_ENV: &str = "FAKE_ENGINE_DATA";
const ORIGINAL: &str = "original";
const MIGRATED: &str = "migrated by the new version";

/// What the bundle this executable runs from says in its Info.plist.
fn this_version() -> String {
    let exe = std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
    // `<bundle>/Contents/MacOS/onicode`.
    let bundle = exe.ancestors().nth(3).unwrap();
    onicode_update::bundle_app_version(bundle).unwrap()
}

fn docs(data: &Path) -> PathBuf {
    data.join("orgs/org/user/docs.sqlite3")
}

fn migrate(data: &Path) {
    std::fs::write(docs(data), MIGRATED).unwrap();
    std::fs::write(docs(data).with_file_name("docs.sqlite3-wal"), "a new wal").unwrap();
}

/// The engine: settle what's outstanding; on trial, do what this bundle's
/// `behavior` file says; otherwise note that it's up, as a normal start would be.
#[test]
fn fake_engine() {
    let Some(data) = std::env::var_os(DATA_ENV).map(PathBuf::from) else {
        return;
    };
    let exe = std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
    // Before anything can move this executable (a commit does).
    let version = this_version();
    let options = BootOptions {
        watchdog_args: [
            "--exact",
            "fake_watchdog",
            "--test-threads=1",
            "--nocapture",
        ]
        .map(Into::into)
        .to_vec(),
        engine_args: std::env::args_os().skip(1).collect(),
    };
    let booted = transaction::boot(&data, &options, || -> anyhow::Result<()> { Ok(()) });
    if let Err(err) = booted {
        eprintln!("boot refused: {err:#}");
        std::process::exit(4);
    }
    if transaction::in_trial() {
        // Beside the exe.
        let behavior = std::fs::read_to_string(exe.with_file_name("behavior")).unwrap();
        match behavior.trim() {
            "ok" => {
                migrate(&data);
                transaction::commit_trial().unwrap();
            }
            "exit-1" => std::process::exit(1),
            "exit-0" => std::process::exit(0),
            "migrate-then-exit" => {
                migrate(&data);
                std::process::exit(1);
            }
            "gives-up" => {
                migrate(&data);
                transaction::note_trial_failure(&data, "It couldn't reach anything.");
                std::process::exit(75);
            }
            "hang" => {
                migrate(&data);
                // Long past any deadline here, but finite: if the watchdog ever
                // stops working this fails rather than holding the suite up.
                std::thread::sleep(Duration::from_secs(45));
                std::process::exit(0);
            }
            other => panic!("unknown behavior {other}"),
        }
    }
    use std::io::Write as _;
    let mut ran = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(data.join("ran"))
        .unwrap();
    writeln!(ran, "{version}").unwrap();
    std::process::exit(0);
}

#[test]
fn fake_watchdog() {
    if std::env::var_os("ONICODE_UPDATE_WATCH_ID").is_none() {
        return;
    }
    transaction::run_watchdog().unwrap();
    std::process::exit(0);
}

/// A macOS app bundle holding a copy of this test binary. A copy, not a hard
/// link: which path a process's image goes by decides things here.
fn write_bundle(bundle: &Path, version: &str, behavior: &str) {
    let dir = bundle.join("Contents/MacOS");
    std::fs::create_dir_all(&dir).unwrap();
    let exe = std::env::current_exe().unwrap();
    std::fs::copy(&exe, dir.join("onicode")).unwrap();
    std::fs::write(dir.join("behavior"), behavior).unwrap();
    std::fs::write(
        bundle.join("Contents/Info.plist"),
        format!("<plist><dict><key>CFBundleShortVersionString</key>\n<string>{version}</string></dict></plist>"),
    )
    .unwrap();
}

struct Rig {
    _dir: tempfile::TempDir,
    data: PathBuf,
    /// The installed app, 1.0.0.
    bundle: PathBuf,
    /// The new one beside it, 2.0.0, doing what `behavior` says on trial.
    incoming: PathBuf,
}

impl Rig {
    fn new(behavior: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let applications = root.join("Applications");
        let rig = Self {
            _dir: dir,
            data: root.join(".onicode"),
            bundle: applications.join("Onicode.app"),
            incoming: applications.join(".Onicode.app.new-test"),
        };
        std::fs::create_dir_all(docs(&rig.data).parent().unwrap()).unwrap();
        std::fs::write(docs(&rig.data), ORIGINAL).unwrap();
        std::fs::write(
            docs(&rig.data).with_file_name("docs.sqlite3-wal"),
            "the wal",
        )
        .unwrap();
        write_bundle(&rig.bundle, "1.0.0", "ok");
        write_bundle(&rig.incoming, "2.0.0", behavior);
        rig
    }

    fn layout(&self) -> Layout {
        Layout::MacBundle {
            bundle: self.bundle.clone(),
            incoming: self.incoming.clone(),
        }
    }

    fn request(&self, restart: RestartMode, deadline_secs: u64) -> String {
        let requested =
            transaction::request(&self.data, self.layout(), "1.0.0", "2.0.0", restart).unwrap();
        // The real deadline is minutes; these tests have seconds.
        let mut record = requested.clone();
        record.deadline_secs = deadline_secs;
        std::fs::write(
            self.data.join("update-transaction.json"),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        requested.id
    }

    fn service() -> RestartMode {
        RestartMode::Service {
            unit: "no-such-unit-for-this-test.service".into(),
        }
    }

    /// One start of the service, as a service manager makes it: whatever bundle
    /// is installed at that moment.
    fn start(&self, crash_at: Option<&str>) -> std::process::ExitStatus {
        self.start_from(&self.bundle, crash_at)
    }

    /// One start of the engine in `bundle`, installed or not.
    fn start_from(&self, bundle: &Path, crash_at: Option<&str>) -> std::process::ExitStatus {
        let mut command = Command::new(bundle.join("Contents/MacOS/onicode"));
        command
            .args(["--exact", "fake_engine", "--test-threads=1", "--nocapture"])
            .env(DATA_ENV, &self.data)
            .env_remove("ONICODE_UPDATE_CRASH_AT")
            .stdout(std::process::Stdio::null());
        if let Some(step) = crash_at {
            command.env("ONICODE_UPDATE_CRASH_AT", step);
        }
        let status = command.status().unwrap();
        eprintln!(
            "start ({crash_at:?}) ended {status:?}, phase {:?}",
            transaction::read(&self.data).map(|t| t.phase)
        );
        status
    }

    /// Keep restarting, as a service manager would, until the update is settled
    /// and a normal start has happened.
    fn run_until_settled(&self) {
        for _ in 0..8 {
            self.start(None);
            if !self.record().in_progress() && self.last_ran().is_some() {
                return;
            }
        }
        panic!(
            "never settled: {:?} ran {:?}",
            self.record().phase,
            self.last_ran()
        );
    }

    fn record(&self) -> transaction::Transaction {
        transaction::read(&self.data).expect("a record")
    }

    fn installed(&self) -> String {
        onicode_update::bundle_app_version(&self.bundle).unwrap()
    }

    fn last_ran(&self) -> Option<String> {
        std::fs::read_to_string(self.data.join("ran"))
            .ok()?
            .lines()
            .last()
            .map(str::to_owned)
    }

    fn database(&self) -> String {
        std::fs::read_to_string(docs(&self.data)).unwrap()
    }

    #[track_caller]
    fn assert_updated(&self, id: &str) {
        let record = self.record();
        assert_eq!(record.id, id);
        assert_eq!(record.phase, Phase::Updated, "{:?}", record.reason);
        assert_eq!(self.installed(), "2.0.0");
        assert_eq!(self.last_ran().as_deref(), Some("2.0.0"));
        assert_eq!(self.database(), MIGRATED);
        assert!(
            !self.data.join("update-snapshots").exists(),
            "backup left behind"
        );
        self.assert_no_bundles_left();
    }

    /// Set aside or half-installed bundles go once nothing runs from them.
    #[track_caller]
    fn assert_no_bundles_left(&self) {
        let left = || -> Vec<String> {
            std::fs::read_dir(self.bundle.parent().unwrap())
                .unwrap()
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name != "Onicode.app")
                .collect()
        };
        // What the engine does a moment after committing: at the commit itself
        // the old version's watchdog is still on its way out of the old bundle.
        let waited = Instant::now();
        while !left().is_empty() && waited.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(100));
            transaction::tidy_latest(&self.data);
        }
        assert!(left().is_empty(), "left behind: {:?}", left());
    }

    #[track_caller]
    fn assert_back_on_the_old_version(&self, id: &str, phase: Phase) {
        let record = self.record();
        assert_eq!(record.id, id);
        assert_eq!(record.phase, phase, "{:?}", record.reason);
        assert!(record.reason.is_some(), "no reason given");
        assert_eq!(self.installed(), "1.0.0");
        assert_eq!(self.last_ran().as_deref(), Some("1.0.0"));
        assert_eq!(self.database(), ORIGINAL);
        assert_eq!(
            std::fs::read_to_string(docs(&self.data).with_file_name("docs.sqlite3-wal")).unwrap(),
            "the wal"
        );
        assert!(
            !self.data.join("update-snapshots").exists(),
            "backup left behind"
        );
        self.assert_no_bundles_left();
    }
}

#[test]
fn a_good_release_swaps_in_and_keeps_its_migration() {
    let rig = Rig::new("ok");
    let id = rig.request(Rig::service(), 60);
    rig.run_until_settled();
    rig.assert_updated(&id);
    assert_eq!(
        rig.record().report().state,
        onicode_update::UpdateState::Updated
    );
}

#[test]
fn a_new_binary_that_exits_at_once_goes_back() {
    let rig = Rig::new("exit-1");
    let id = rig.request(Rig::service(), 60);
    rig.run_until_settled();
    rig.assert_back_on_the_old_version(&id, Phase::RolledBack);
    assert_eq!(
        rig.record().report().state,
        onicode_update::UpdateState::RolledBack
    );
}

/// A clean exit is still a failed trial, whatever a service manager makes of it.
#[test]
fn a_new_binary_that_exits_cleanly_goes_back() {
    let rig = Rig::new("exit-0");
    let id = rig.request(Rig::service(), 60);
    rig.run_until_settled();
    rig.assert_back_on_the_old_version(&id, Phase::RolledBack);
}

#[test]
fn a_migration_is_undone_when_the_new_binary_then_dies() {
    let rig = Rig::new("migrate-then-exit");
    let id = rig.request(Rig::service(), 60);
    let first = rig.start(None);
    assert_eq!(first.code(), Some(1));
    assert_eq!(
        rig.database(),
        MIGRATED,
        "the trial should have got that far"
    );
    assert_eq!(rig.installed(), "1.0.0", "swapped in before the commit");
    rig.run_until_settled();
    rig.assert_back_on_the_old_version(&id, Phase::RolledBack);
}

#[test]
fn a_trials_own_reason_reaches_the_record() {
    let rig = Rig::new("gives-up");
    let id = rig.request(Rig::service(), 60);
    rig.run_until_settled();
    rig.assert_back_on_the_old_version(&id, Phase::RolledBack);
    let reason = rig.record().reason.unwrap();
    assert!(reason.contains("It couldn't reach anything."), "{reason}");
    assert!(reason.contains("went back to 1.0.0"), "{reason}");
}

/// The hang: only the watchdog can end it. It's a sibling of the real process,
/// from the old version, and it kills the new one at the deadline.
#[test]
fn a_new_binary_that_hangs_is_stopped_at_the_deadline_and_goes_back() {
    let rig = Rig::new("hang");
    let id = rig.request(Rig::service(), 2);
    let started = Instant::now();
    let hung = rig.start(None);
    let waited = started.elapsed();
    use std::os::unix::process::ExitStatusExt as _;
    assert_eq!(hung.signal(), Some(libc::SIGKILL), "{hung:?}");
    assert!(
        waited >= Duration::from_secs(2),
        "killed too soon: {waited:?}"
    );
    assert!(
        waited < Duration::from_secs(20),
        "killed too late: {waited:?}"
    );
    rig.run_until_settled();
    rig.assert_back_on_the_old_version(&id, Phase::RolledBack);
    let reason = rig.record().reason.unwrap();
    assert!(reason.contains("didn't finish starting"), "{reason}");
}

/// The desktop app starts its engine when the port goes quiet. Doing that during
/// a trial must not be taken for the trial having failed.
#[test]
fn a_second_start_during_a_trial_leaves_the_trial_alone() {
    let rig = Rig::new("hang");
    // Long enough that a slow machine's second start still lands inside it.
    let id = rig.request(Rig::service(), 8);
    std::thread::scope(|scope| {
        let trial = scope.spawn(|| rig.start(None));
        let waited = Instant::now();
        while rig.record().phase != Phase::Trial || rig.database() != MIGRATED {
            assert!(
                waited.elapsed() < Duration::from_secs(10),
                "the trial never started"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let second = rig.start(None);
        assert_eq!(second.code(), Some(4), "{second:?}");
        assert_eq!(rig.record().phase, Phase::Trial);
        assert_eq!(
            rig.database(),
            MIGRATED,
            "it must not have restored under the trial"
        );
        let _ = trial.join();
    });
    rig.run_until_settled();
    rig.assert_back_on_the_old_version(&id, Phase::RolledBack);
}

/// With nothing to restart it (the desktop app closed), the watchdog itself
/// starts the installed app. This test starts nothing after the first start.
#[test]
fn with_no_service_manager_the_watchdog_brings_the_old_version_back() {
    for (behavior, deadline) in [("exit-1", 60), ("exit-0", 60), ("hang", 2)] {
        let rig = Rig::new(behavior);
        let id = rig.request(RestartMode::Relaunch, deadline);
        rig.start(None);
        let waited = Instant::now();
        while rig.last_ran().is_none() {
            assert!(
                waited.elapsed() < Duration::from_secs(30),
                "{behavior}: nothing came back"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        rig.assert_back_on_the_old_version(&id, Phase::RolledBack);
    }
}

/// A crash at each step of a good update. Up to the point where the new version
/// has been started, starting again simply carries on; once it has, the old
/// version can't know how far it got and goes back; from the moment the bundles
/// swap, it's done.
#[test]
fn a_crash_at_any_step_of_a_good_update_lands_somewhere_whole() {
    enum Lands {
        Updated,
        Back,
    }
    for (step, lands) in [
        ("snapshot-partial", Lands::Updated),
        ("snapshot-done", Lands::Updated),
        ("trial-marked", Lands::Back),
        ("watchdog-started", Lands::Back),
        ("trial-before-commit", Lands::Back),
        ("commit-flipped", Lands::Updated),
        ("commit-recorded", Lands::Updated),
    ] {
        let rig = Rig::new("ok");
        let id = rig.request(Rig::service(), 60);
        let crashed = rig.start(Some(step));
        assert!(!crashed.success(), "{step}: it should have crashed");
        rig.run_until_settled();
        match lands {
            Lands::Updated => rig.assert_updated(&id),
            Lands::Back => rig.assert_back_on_the_old_version(&id, Phase::RolledBack),
        }
    }
}

/// A crash at each step of going back: the restore finishes before anything
/// opens the databases, and the record ends up true.
#[test]
fn a_crash_at_any_step_of_going_back_still_gets_back() {
    for step in [
        "restore-marked",
        "restore-partial",
        "restored",
        "rollback-recorded",
    ] {
        let rig = Rig::new("migrate-then-exit");
        let id = rig.request(Rig::service(), 60);
        assert_eq!(rig.start(None).code(), Some(1));
        let crashed = rig.start(Some(step));
        assert!(!crashed.success(), "{step}: it should have crashed");
        if step != "rollback-recorded" {
            assert_eq!(rig.record().phase, Phase::Restoring, "{step}");
        }
        rig.run_until_settled();
        rig.assert_back_on_the_old_version(&id, Phase::RolledBack);
    }
}

/// The installed bundle is what boots, so the new one, started by hand from
/// beside it, can't run while the old one has a restore to finish.
#[test]
fn the_new_version_refuses_to_start_during_a_restore() {
    let rig = Rig::new("migrate-then-exit");
    let id = rig.request(Rig::service(), 60);
    assert_eq!(rig.start(None).code(), Some(1));
    rig.start(Some("restore-partial"));
    assert_eq!(rig.record().phase, Phase::Restoring);
    let refused = rig.start_from(&rig.incoming, None);
    assert_eq!(refused.code(), Some(4), "{refused:?}");
    assert_eq!(rig.record().phase, Phase::Restoring);
    rig.run_until_settled();
    rig.assert_back_on_the_old_version(&id, Phase::RolledBack);
}

#[test]
fn release_files_gone_missing_leave_it_on_the_old_version_untouched() {
    let rig = Rig::new("ok");
    let id = rig.request(Rig::service(), 60);
    std::fs::remove_dir_all(&rig.incoming).unwrap();
    rig.run_until_settled();
    rig.assert_back_on_the_old_version(&id, Phase::Failed);
    assert_eq!(
        rig.record().report().state,
        onicode_update::UpdateState::Failed
    );
}

/// A binary that can't even be started: it never ran, so nothing was restored
/// and there's no second start to wait for.
#[test]
fn a_new_binary_that_cannot_be_started_leaves_it_on_the_old_version() {
    use std::os::unix::fs::PermissionsExt as _;
    let rig = Rig::new("ok");
    let id = rig.request(Rig::service(), 60);
    let broken = rig.incoming.join("Contents/MacOS/onicode");
    std::fs::remove_file(&broken).unwrap();
    std::fs::write(&broken, "not a program").unwrap();
    std::fs::set_permissions(&broken, std::fs::Permissions::from_mode(0o644)).unwrap();
    let only = rig.start(None);
    assert!(only.success(), "{only:?}");
    rig.assert_back_on_the_old_version(&id, Phase::Failed);
}

/// Another version was installed mid-update, the way the app installs one
/// (`apply_mac_app`: the old bundle set aside, the new one in its place). That
/// version owns the data now, and restoring over it could lose what it wrote.
#[test]
fn a_third_version_installed_meanwhile_is_left_alone() {
    let rig = Rig::new("migrate-then-exit");
    let id = rig.request(Rig::service(), 60);
    assert_eq!(rig.start(None).code(), Some(1));
    std::fs::rename(
        &rig.bundle,
        rig.bundle.with_file_name(".Onicode.app.old-installer"),
    )
    .unwrap();
    write_bundle(&rig.bundle, "3.0.0", "ok");
    rig.run_until_settled();
    let record = rig.record();
    assert_eq!(record.id, id);
    assert_eq!(record.phase, Phase::Failed);
    assert!(record.reason.is_some(), "no reason given");
    assert_eq!(rig.installed(), "3.0.0");
    assert_eq!(rig.last_ran().as_deref(), Some("3.0.0"));
    assert_eq!(
        rig.database(),
        MIGRATED,
        "it must not restore over another version's data"
    );
    assert!(
        !rig.data.join("update-snapshots").exists(),
        "backup left behind"
    );
    rig.assert_no_bundles_left();
}

/// #1037: a bundle something runs from isn't deleted, however finished with
/// it the update is, and goes once nothing does.
#[test]
fn a_bundle_in_use_is_kept_until_nothing_runs_from_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let bundle = root.join("Onicode.app");
    let retired = root.join(".Onicode.app.old-123");
    std::fs::create_dir_all(&bundle).unwrap();
    std::fs::create_dir_all(retired.join("Contents/MacOS")).unwrap();
    let sleeper = retired.join("Contents/MacOS/sleeper");
    let source = root.join("sleep.c");
    std::fs::write(
        &source,
        "#include <unistd.h>\nint main(void){sleep(30);return 0;}\n",
    )
    .unwrap();
    let compiled = Command::new("cc")
        .arg("-o")
        .arg(&sleeper)
        .arg(&source)
        .status();
    if !compiled.is_ok_and(|status| status.success()) {
        eprintln!("no C compiler: skipping");
        return;
    }
    let mut running = Command::new(&sleeper).spawn().unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert!(onicode_update::process_runs_from(&retired));
    onicode_update::prune_retired_mac_bundles(&bundle);
    assert!(retired.exists(), "deleted from under a running process");

    running.kill().unwrap();
    running.wait().unwrap();
    assert!(!onicode_update::process_runs_from(&retired));
    onicode_update::prune_retired_mac_bundles(&bundle);
    assert!(!retired.exists(), "left behind once nothing ran from it");
    assert!(bundle.exists());
}
