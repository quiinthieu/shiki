#[path = "../../../tests/support/pty.rs"]
mod pty;
#[path = "../../../tests/support/mod.rs"]
mod support;

use assert_cmd::Command;
use proptest::prelude::*;
use pty::PtyPair;
use sk_core::palette::{compute_frame, ModelConfig, Palette};
use sk_core::product;
use sk_core::solar::Coordinates;
use sk_core::time::UtcInstant;
use sk_deliver::osc::{contains_palette_mutating_osc, encode_osc_frame};
use sk_store::store::{expire_generation_freshness, load_generation_record, load_state};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::Duration;
use support::TestEnv;

const PTY_TIMEOUT: Duration = Duration::from_secs(10);

fn shiki_bin_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_shiki-term"))
}

fn shiki_cmd(env: &TestEnv) -> Command {
    let mut cmd = Command::new(shiki_bin_path());
    cmd.envs(env.env_vars());
    cmd.env_remove("ZDOTDIR");
    cmd.env_remove("NO_COLOR");
    cmd.env_remove("COLORTERM");
    cmd.env("TERM", "vt100");
    cmd
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

fn path_with_shiki_bin() -> OsString {
    let bin_dir = shiki_bin_path()
        .parent()
        .expect("shiki-term binary parent dir")
        .to_path_buf();
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![bin_dir];
    paths.extend(std::env::split_paths(&existing));
    std::env::join_paths(paths).expect("join PATH")
}

fn find_executable_on_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[test]
fn version_reports_mvp_v0_1_0() {
    let env = TestEnv::new();
    let out = shiki_cmd(&env)
        .arg("--version")
        .output()
        .expect("run shiki-term --version");
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("{} 0.1.0", product::CLI_BIN)
    );
}

#[test]
fn hook_scripts_match_golden_snapshots_and_emit_zero_osc_bytes() {
    let env = TestEnv::new();
    for shell in ["zsh", "bash", "fish"] {
        let out = shiki_cmd(&env)
            .args(["hook", shell])
            .output()
            .unwrap_or_else(|e| panic!("run hook {shell}: {e}"));
        assert!(
            out.status.success(),
            "hook {shell} stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !contains_palette_mutating_osc(&out.stdout),
            "`hook {shell}` output must be pure shell code with zero OSC palette mutations"
        );
        let script = String::from_utf8(out.stdout).expect("valid UTF-8 hook script");
        insta::assert_snapshot!(format!("hook_script_{shell}"), script);
    }
}

#[test]
fn emit_if_stale_is_silent_on_stdout_and_updates_generation_only_on_change() {
    let env = TestEnv::new();
    shiki_cmd(&env)
        .args(["locate", "--set", "35.7,139.7"])
        .assert()
        .success();

    // 1. Cold start at Tokyo midnight (`2026-03-20T15:00:00Z`) initializes `current.seq` and `current.gen`.
    let out1 = shiki_cmd(&env)
        .args(["_emit", "--if-stale", "--time", "2026-03-20T15:00:00Z"])
        .output()
        .expect("run _emit --if-stale cold");
    assert!(
        out1.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out1.stderr)
    );
    assert!(
        out1.stdout.is_empty(),
        "_emit must never write OSC bytes to its own stdout"
    );

    let coords = Coordinates::new(35.7, 139.7).unwrap();
    let palette = Palette::bundled_shiki();
    let model = ModelConfig::bundled();
    let night_osc = encode_osc_frame(
        &compute_frame(
            UtcInstant::parse_rfc3339("2026-03-20T15:00:00Z").unwrap(),
            coords,
            &palette,
            &model,
        )
        .colors,
    );
    assert_eq!(fs::read(env.paths().current_seq_file()).unwrap(), night_osc);
    let gen1 = load_generation_record(env.paths()).unwrap().unwrap();
    assert_eq!(gen1.generation, 1);

    // 2. Within `hook_min_interval_secs` (20s later, even if querying dawn!), `--if-stale` does not recompute.
    shiki_cmd(&env)
        .args(["_emit", "--if-stale", "--time", "2026-03-20T15:00:20Z"])
        .assert()
        .success();
    assert_eq!(
        load_generation_record(env.paths())
            .unwrap()
            .unwrap()
            .generation,
        1
    );

    // 3. Past `hook_min_interval_secs` (90s later, still deep night), recomputes once without bumping generation.
    shiki_cmd(&env)
        .args(["_emit", "--if-stale", "--time", "2026-03-20T15:01:30Z"])
        .assert()
        .success();
    let gen_after_night = load_generation_record(env.paths()).unwrap().unwrap();
    assert_eq!(gen_after_night.generation, 1);

    // 4. At Tokyo dawn (`2026-03-20T20:45:00Z`), colours change above `step_threshold` -> generation 2.
    shiki_cmd(&env)
        .args(["_emit", "--if-stale", "--time", "2026-03-20T20:45:00Z"])
        .assert()
        .success();
    let gen2 = load_generation_record(env.paths()).unwrap().unwrap();
    assert_eq!(gen2.generation, 2);
    let dawn_osc = encode_osc_frame(
        &compute_frame(
            UtcInstant::parse_rfc3339("2026-03-20T20:45:00Z").unwrap(),
            coords,
            &palette,
            &model,
        )
        .colors,
    );
    assert_eq!(fs::read(env.paths().current_seq_file()).unwrap(), dawn_osc);
    assert_ne!(dawn_osc, night_osc);
}

#[test]
fn hook_install_requires_write_rc_flag_and_creates_backup_and_round_trips_byte_for_byte() {
    let env = TestEnv::new();
    let zshrc = env.home_dir().join(".zshrc");
    let bashrc = env.home_dir().join(".bashrc");
    let fish_cfg = env.xdg_config_home().join("fish").join("config.fish");

    // 1. Without `--write-rc`, `hook install` refuses to touch the filesystem (`PLAN.md` §0 #6).
    let refused = shiki_cmd(&env)
        .args(["hook", "zsh", "install"])
        .output()
        .expect("run hook zsh install without --write-rc");
    assert!(!refused.status.success());
    assert!(!zshrc.exists());

    let refused2 = shiki_cmd(&env)
        .args(["hook", "install", "zsh"])
        .output()
        .expect("run hook install zsh without --write-rc");
    assert!(!refused2.status.success());
    assert!(!zshrc.exists());

    // 2. Non-existent rc file: `install --write-rc` creates it; `uninstall` removes it cleanly with zero residue.
    shiki_cmd(&env)
        .args(["hook", "fish", "install", "--write-rc"])
        .assert()
        .success();
    assert!(fish_cfg.exists());
    shiki_cmd(&env)
        .args(["hook", "fish", "uninstall"])
        .assert()
        .success();
    assert!(
        !fish_cfg.exists(),
        "uninstalling a Shiki-created empty rc file must leave zero residue"
    );

    // 3. Existing rc files across edge cases:
    //    - normal trailing `\n` + UTF-8 comments + custom permissions (`0o644`)
    //    - NO trailing newline at EOF (`bashrc`)
    //    - existing 0-byte empty file
    let original_zshrc =
        b"# \xE5\x9B\x9B\xE5\xAD\xA3 user config\nexport EDITOR=vim\nalias ll='ls -la'\n";
    fs::write(&zshrc, original_zshrc).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&zshrc, fs::Permissions::from_mode(0o644)).unwrap();
    }

    shiki_cmd(&env)
        .args(["hook", "install", "zsh", "--write-rc"])
        .assert()
        .success();

    let backup_zshrc = env
        .home_dir()
        .join(format!(".zshrc.{}.bak", product::CLI_BIN));
    assert!(
        backup_zshrc.exists(),
        "install --write-rc must create a backup file"
    );
    assert_eq!(fs::read(&backup_zshrc).unwrap(), original_zshrc);

    let installed_once = fs::read(&zshrc).unwrap();
    assert_ne!(installed_once, original_zshrc);

    // Idempotency: running `hook zsh install --write-rc` a second time leaves the file identical
    // and preserves the original pre-Shiki backup!
    shiki_cmd(&env)
        .args(["hook", "zsh", "install", "--write-rc"])
        .assert()
        .success();
    assert_eq!(fs::read(&zshrc).unwrap(), installed_once);
    assert_eq!(fs::read(&backup_zshrc).unwrap(), original_zshrc);

    // Uninstall restores exact bytes, preserves Unix mode `0o644`, and removes `.bak` residue.
    shiki_cmd(&env)
        .args(["hook", "uninstall", "zsh"])
        .assert()
        .success();
    assert_eq!(fs::read(&zshrc).unwrap(), original_zshrc);
    assert!(!backup_zshrc.exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&zshrc).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
    }

    // 4. Existing rc file WITHOUT trailing newline + `$SHELL` auto-detection when `<shell>` omitted.
    let original_bashrc_no_nl = b"export PATH=\"$HOME/bin:$PATH\"";
    fs::write(&bashrc, original_bashrc_no_nl).unwrap();
    shiki_cmd(&env)
        .env("SHELL", "/bin/bash")
        .args(["hook", "install", "--write-rc"])
        .assert()
        .success();
    shiki_cmd(&env)
        .env("SHELL", "/bin/bash")
        .args(["hook", "uninstall"])
        .assert()
        .success();
    assert_eq!(fs::read(&bashrc).unwrap(), original_bashrc_no_nl);

    // 5. User edits added AFTER `install --write-rc` are preserved on `uninstall`.
    fs::write(&zshrc, "export BEFORE=1\n").unwrap();
    shiki_cmd(&env)
        .args(["hook", "zsh", "install", "--write-rc"])
        .assert()
        .success();
    let mut with_after = fs::read_to_string(&zshrc).unwrap();
    with_after.push_str("export AFTER=2\n");
    fs::write(&zshrc, with_after).unwrap();

    shiki_cmd(&env)
        .args(["hook", "zsh", "uninstall"])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(&zshrc).unwrap(),
        "export BEFORE=1\nexport AFTER=2\n"
    );

    // 6. Existing 0-byte empty file stays an existing 0-byte file after `install -> uninstall`.
    fs::write(&zshrc, b"").unwrap();
    shiki_cmd(&env)
        .args(["hook", "zsh", "install", "--write-rc"])
        .assert()
        .success();
    shiki_cmd(&env)
        .args(["hook", "zsh", "uninstall"])
        .assert()
        .success();
    assert!(zshrc.exists());
    assert!(fs::read(&zshrc).unwrap().is_empty());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn prop_rc_file_round_trips_byte_for_byte(
        lines in prop::collection::vec("[a-zA-Z0-9_='# /.-]{0,40}", 0..8),
        line_ending in prop_oneof![Just("\n"), Just("\r\n")],
        trailing_newlines in 0usize..=3,
    ) {
        // Guard against random input accidentally synthesizing Shiki's marker delimiters.
        let mut original = lines.join(line_ending);
        for _ in 0..trailing_newlines {
            original.push_str(line_ending);
        }
        prop_assume!(!original.contains(">>> ") && !original.contains("<<< "));

        let env = TestEnv::new();
        let zshrc = env.home_dir().join(".zshrc");
        fs::write(&zshrc, original.as_bytes()).unwrap();

        shiki_cmd(&env)
            .args(["hook", "zsh", "install", "--write-rc"])
            .assert()
            .success();
        shiki_cmd(&env)
            .args(["hook", "zsh", "uninstall"])
            .assert()
            .success();

        let restored = fs::read(&zshrc).unwrap();
        prop_assert_eq!(restored, original.as_bytes());
    }
}

fn opposite_phase_timestamp(current_osc: &[u8]) -> &'static str {
    let coords = Coordinates::new(35.7, 139.7).unwrap();
    let palette = Palette::bundled_shiki();
    let model = ModelConfig::bundled();

    let noon_ts = "2026-06-21T03:00:00Z";
    let noon_osc = encode_osc_frame(
        &compute_frame(
            UtcInstant::parse_rfc3339(noon_ts).unwrap(),
            coords,
            &palette,
            &model,
        )
        .colors,
    );
    if noon_osc != current_osc {
        noon_ts
    } else {
        "2026-12-21T15:00:00Z"
    }
}

#[cfg(unix)]
fn exercise_real_shell_on_pty(
    shell_name: &str,
    build_shell_cmd: impl Fn(&TestEnv, &Path) -> StdCommand,
    prepare_rc: impl Fn(&TestEnv) -> PathBuf,
) {
    let env = TestEnv::new();
    shiki_cmd(&env)
        .args(["locate", "--set", "35.7,139.7"])
        .assert()
        .success();

    let rc_path = prepare_rc(&env);
    shiki_cmd(&env)
        .args(["hook", shell_name, "install", "--write-rc"])
        .assert()
        .success();
    assert!(rc_path.exists());

    // Wrap `SHIKI_TERM_BIN` so we can count exact `_emit --if-stale` subprocess spawns across prompts.
    let emit_count_file = env.home_dir().join("emit_spawns.log");
    let wrapper_path = env.home_dir().join("shiki-wrapper.sh");
    fs::write(
        &wrapper_path,
        format!(
            "#!/bin/sh\nprintf '1\\n' >> \"{}\"\nexec \"{}\" \"$@\"\n",
            emit_count_file.display(),
            shiki_bin_path().display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&wrapper_path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let count_spawns = || -> usize {
        fs::read_to_string(&emit_count_file)
            .map(|s| s.lines().count())
            .unwrap_or(0)
    };
    let make_shell = || {
        let mut cmd = build_shell_cmd(&env, &rc_path);
        cmd.env("SHIKI_TERM_BIN", &wrapper_path);
        cmd
    };

    let mut pty = PtyPair::open_cooked().expect("open cooked PTY");
    let mut child = pty
        .spawn_session_command(make_shell())
        .unwrap_or_else(|e| panic!("spawn {shell_name} on PTY: {e}"));

    // 1. First prompt: cold start runs `_emit --if-stale`, populates `current.seq` & `current.gen`,
    //    and emits the exact OSC frame bytes before prompt 1!
    let p1 = pty
        .read_until(b"<<SHIKI_PROMPT:0>> ", PTY_TIMEOUT)
        .unwrap_or_else(|e| panic!("{shell_name} prompt 1 timed out: {e}"));
    let expected_osc_1 = fs::read(env.paths().current_seq_file()).expect("current.seq after p1");
    assert!(!expected_osc_1.is_empty());
    assert!(
        contains_palette_mutating_osc(&p1),
        "{shell_name} must emit OSC palette sequences on the first prompt"
    );
    assert!(
        contains_subslice(&p1, &expected_osc_1),
        "{shell_name} first prompt must contain exact current.seq bytes"
    );
    assert_eq!(load_state(env.paths()).unwrap().generation, 1);
    assert_eq!(
        count_spawns(),
        1,
        "{shell_name} cold start must spawn _emit once"
    );

    // 2. Second prompt (unchanged, fast path): zero OSC bytes emitted, and zero subprocess spawns in zsh/bash!
    pty.write_input(b"true\n").unwrap();
    let p2 = pty
        .read_until(b"<<SHIKI_PROMPT:0>> ", PTY_TIMEOUT)
        .unwrap_or_else(|e| panic!("{shell_name} prompt 2 timed out: {e}"));
    assert!(
        !contains_palette_mutating_osc(&p2),
        "{shell_name} must emit zero OSC sequences on unchanged prompt 2"
    );
    if matches!(shell_name, "zsh" | "bash") {
        assert_eq!(
            count_spawns(),
            1,
            "{shell_name} fast path must not fork `_emit --if-stale` within hook_min_interval_secs"
        );
    }

    // 3. Third prompt: expire freshness lease so `_emit --if-stale` actually executes,
    //    recomputes at the same wall-clock moment (`ΔE_OK < step_threshold`), keeps generation 1,
    //    and STILL emits zero OSC bytes!
    let spawns_before_p3 = count_spawns();
    expire_generation_freshness(env.paths()).unwrap();
    pty.write_input(b"true\n").unwrap();
    let p3 = pty
        .read_until(b"<<SHIKI_PROMPT:0>> ", PTY_TIMEOUT)
        .unwrap_or_else(|e| panic!("{shell_name} prompt 3 timed out: {e}"));
    assert!(
        !contains_palette_mutating_osc(&p3),
        "{shell_name} must emit zero OSC sequences when stale check sees no colour change"
    );
    assert_eq!(load_state(env.paths()).unwrap().generation, 1);
    assert_eq!(
        count_spawns(),
        spawns_before_p3 + 1,
        "{shell_name} must re-invoke `_emit --if-stale` when current.gen freshness expires"
    );

    // 4. Fourth prompt: shift store to an opposite solar phase (`apply --time ...`), bumping
    //    `current.gen` to 2. The next prompt in the live shell emits the exact new OSC bytes!
    let next_ts = opposite_phase_timestamp(&expected_osc_1);
    shiki_cmd(&env)
        .args(["apply", "--time", next_ts])
        .assert()
        .success();
    let expected_osc_2 = fs::read(env.paths().current_seq_file()).expect("current.seq gen 2");
    assert_ne!(expected_osc_2, expected_osc_1);
    assert_eq!(load_state(env.paths()).unwrap().generation, 2);

    pty.write_input(b"true\n").unwrap();
    let p4 = pty
        .read_until(b"<<SHIKI_PROMPT:0>> ", PTY_TIMEOUT)
        .unwrap_or_else(|e| panic!("{shell_name} prompt 4 timed out: {e}"));
    assert!(
        contains_palette_mutating_osc(&p4),
        "{shell_name} must emit new OSC sequences when generation advances"
    );
    assert!(
        contains_subslice(&p4, &expected_osc_2),
        "{shell_name} prompt 4 must contain exact updated current.seq bytes"
    );

    // 5. Fifth prompt (caught up to generation 2) + exit status `$?`/`$status` preservation (`42`)!
    pty.write_input(b"sh -c 'exit 42'\n").unwrap();
    let p5 = pty
        .read_until(b"<<SHIKI_PROMPT:42>> ", PTY_TIMEOUT)
        .unwrap_or_else(|e| {
            panic!("{shell_name} prompt 5 timed out (hook must preserve exit code 42): {e}")
        });
    assert!(
        !contains_palette_mutating_osc(&p5),
        "{shell_name} must emit zero OSC sequences on prompt 5 once caught up to generation 2"
    );

    pty.write_input(b"exit\n").unwrap();
    let _ = pty.read_to_eof(PTY_TIMEOUT);
    let status = child
        .wait_with_timeout(PTY_TIMEOUT)
        .expect("reap first shell child");
    assert!(status.success() || status.code() == Some(42));

    // 6. Brand-new shell session (second terminal tab): catches up on its first prompt, silent on second!
    let mut pty2 = PtyPair::open_cooked().expect("open second cooked PTY");
    let mut child2 = pty2
        .spawn_session_command(make_shell())
        .expect("spawn second shell");
    let tab2_p1 = pty2
        .read_until(b"<<SHIKI_PROMPT:0>> ", PTY_TIMEOUT)
        .expect("tab2 prompt 1");
    assert!(contains_subslice(&tab2_p1, &expected_osc_2));

    pty2.write_input(b"true\n").unwrap();
    let tab2_p2 = pty2
        .read_until(b"<<SHIKI_PROMPT:0>> ", PTY_TIMEOUT)
        .expect("tab2 prompt 2");
    assert!(!contains_palette_mutating_osc(&tab2_p2));

    pty2.write_input(b"exit\n").unwrap();
    let _ = pty2.read_to_eof(PTY_TIMEOUT);
    let _ = child2.wait_with_timeout(PTY_TIMEOUT).unwrap();
}

#[cfg(unix)]
#[test]
fn real_zsh_on_pty_emits_on_first_prompt_and_afterwards_only_on_change() {
    let Some(zsh_bin) = find_executable_on_path("zsh") else {
        panic!("zsh must be available on Unix test hosts");
    };
    let path_env = path_with_shiki_bin();
    exercise_real_shell_on_pty(
        "zsh",
        |env, _rc| {
            let mut cmd = StdCommand::new(&zsh_bin);
            cmd.args(["-d", "+Z", "-i"]);
            cmd.env_clear();
            cmd.envs(env.env_vars());
            cmd.env("ZDOTDIR", env.home_dir());
            cmd.env("PATH", &path_env);
            cmd.env("TERM", "vt100");
            cmd
        },
        |env| {
            let rc = env.home_dir().join(".zshrc");
            fs::write(&rc, "unsetopt PROMPT_SP\nPROMPT='<<SHIKI_PROMPT:%?>> '\n").unwrap();
            rc
        },
    );
}

#[cfg(unix)]
#[test]
fn real_bash_on_pty_emits_on_first_prompt_and_afterwards_only_on_change() {
    let Some(bash_bin) = find_executable_on_path("bash") else {
        panic!("bash must be available on Unix test hosts");
    };
    let path_env = path_with_shiki_bin();
    exercise_real_shell_on_pty(
        "bash",
        |env, rc| {
            let mut cmd = StdCommand::new(&bash_bin);
            cmd.arg("--noprofile")
                .arg("--rcfile")
                .arg(rc)
                .args(["--noediting", "-i"]);
            cmd.env_clear();
            cmd.envs(env.env_vars());
            cmd.env("PATH", &path_env);
            cmd.env("TERM", "vt100");
            cmd
        },
        |env| {
            let rc = env.home_dir().join(".bashrc");
            fs::write(&rc, "PS1='<<SHIKI_PROMPT:$?>> '\n").unwrap();
            rc
        },
    );
}

#[cfg(unix)]
#[test]
fn real_fish_on_pty_emits_on_first_prompt_and_afterwards_only_on_change_when_installed() {
    let Some(fish_bin) = find_executable_on_path("fish") else {
        eprintln!("skipping live fish PTY test: `fish` is not installed on PATH");
        return;
    };
    let path_env = path_with_shiki_bin();
    exercise_real_shell_on_pty(
        "fish",
        |env, _rc| {
            let mut cmd = StdCommand::new(&fish_bin);
            cmd.arg("-i");
            cmd.env_clear();
            cmd.envs(env.env_vars());
            cmd.env("PATH", &path_env);
            cmd.env("TERM", "dumb");
            cmd.env("LC_ALL", "C");
            cmd
        },
        |env| {
            let fish_dir = env.xdg_config_home().join("fish");
            fs::create_dir_all(&fish_dir).unwrap();
            let rc = fish_dir.join("config.fish");
            fs::write(
                &rc,
                "function fish_greeting; end\nfunction fish_title; end\nfunction fish_right_prompt; end\nfunction fish_prompt; set -l s $status; printf '<<SHIKI_PROMPT:%d>> ' $s; end\n",
            )
            .unwrap();
            rc
        },
    );
}
