//! Shell prompt hook generator (`zsh`, `bash`, `fish`) and byte-exact rc file installer (`PLAN.md` §5, §6, §10 T08).

use clap::ValueEnum;
use sk_core::product;
use std::env;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const TAG_TRIM_LEADING_NL: &str = "# shiki-restore: trim-leading-newline";
const TAG_REMOVE_IF_EMPTY: &str = "# shiki-restore: remove-if-empty";

static RC_TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum HookShell {
    Zsh,
    Bash,
    Fish,
}

impl HookShell {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Zsh => "zsh",
            Self::Bash => "bash",
            Self::Fish => "fish",
        }
    }

    /// Detects the user's login shell from `$SHELL` (e.g. `/bin/zsh` -> [`HookShell::Zsh`]).
    pub fn detect_from_env() -> Result<Self, String> {
        let raw = env::var("SHELL").map_err(|_| {
            "unable to detect shell from $SHELL; specify one explicitly (`zsh`, `bash`, or `fish`)"
                .to_string()
        })?;
        Self::from_shell_path(&raw).ok_or_else(|| {
            format!(
                "unsupported shell '{raw}' in $SHELL; specify `zsh`, `bash`, or `fish` explicitly"
            )
        })
    }

    #[must_use]
    pub fn from_shell_path(raw: &str) -> Option<Self> {
        let name = Path::new(raw.trim()).file_name()?.to_str()?;
        match name {
            "zsh" => Some(Self::Zsh),
            "bash" => Some(Self::Bash),
            "fish" => Some(Self::Fish),
            _ => None,
        }
    }

    /// Resolves the target rc file path for this shell, honouring `ZDOTDIR`, `XDG_CONFIG_HOME`, and `HOME`.
    pub fn default_rc_path(self) -> Result<PathBuf, String> {
        let non_empty = |key: &str| {
            env::var_os(key)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        let home = non_empty("HOME")
            .ok_or_else(|| "unable to determine HOME directory for shell rc file".to_string())?;

        match self {
            Self::Zsh => {
                let zdotdir = non_empty("ZDOTDIR").unwrap_or(home);
                Ok(zdotdir.join(".zshrc"))
            }
            Self::Bash => Ok(home.join(".bashrc")),
            Self::Fish => {
                let cfg_base = non_empty("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
                Ok(cfg_base.join("fish").join("config.fish"))
            }
        }
    }
}

impl fmt::Display for HookShell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Renders the prompt-hook script evaluated by `eval "$(<bin> hook <shell>)"` or `<bin> hook fish | source`.
///
/// Fast path (`PLAN.md` §7): reads `current.gen` (`<generation> <fresh_until_unix_secs>`) using pure
/// shell builtins (`$EPOCHSECONDS` in `zsh` / `bash` 5+) so unchanged prompts within
/// `hook_min_interval_secs` perform zero subprocess spawns and zero colour math.
#[must_use]
pub fn render_hook_script(shell: HookShell) -> String {
    let bin = product::CLI_BIN;
    let display = product::DISPLAY_NAME;
    match shell {
        HookShell::Zsh => format!(
            r##"# {display} prompt hook for zsh (`eval "$({bin} hook zsh)"`)
zmodload -F zsh/datetime b:EPOCHSECONDS 2>/dev/null || true
typeset -g __SHIKI_GEN="${{__SHIKI_GEN:-}}"

__shiki_precmd() {{
    local _shiki_status=$?
    [[ -t 1 ]] || return $_shiki_status

    local _shiki_rt _shiki_cfg
    if [[ -n "${{SHIKI_HOME:-}}" ]]; then
        _shiki_rt="$SHIKI_HOME"
        _shiki_cfg="$SHIKI_HOME/config.toml"
    elif [[ "${{OSTYPE:-}}" == darwin* ]]; then
        _shiki_rt="${{HOME:-}}/Library/Application Support/{display}"
        _shiki_cfg="$_shiki_rt/config.toml"
    else
        local _shiki_cfg_dir="${{XDG_CONFIG_HOME:-${{HOME:-}}/.config}}/{bin}"
        local _shiki_state_dir="${{XDG_STATE_HOME:-${{HOME:-}}/.local/state}}/{bin}"
        if [[ -n "${{XDG_RUNTIME_DIR:-}}" ]]; then
            _shiki_rt="$XDG_RUNTIME_DIR/{bin}"
        else
            _shiki_rt="$_shiki_state_dir"
        fi
        _shiki_cfg="$_shiki_cfg_dir/config.toml"
    fi

    local _shiki_gen_file="$_shiki_rt/current.gen"
    local _shiki_seq_file="$_shiki_rt/current.seq"
    local _shiki_gen="" _shiki_until=0 _shiki_fresh=0

    if [[ -r "$_shiki_gen_file" && -e "$_shiki_seq_file" && ! "$_shiki_cfg" -nt "$_shiki_gen_file" ]]; then
        IFS=' ' read -r _shiki_gen _shiki_until < "$_shiki_gen_file" 2>/dev/null || true
        if [[ -n "${{EPOCHSECONDS:-}}" && "$_shiki_until" == <-> ]] && (( EPOCHSECONDS < _shiki_until )); then
            _shiki_fresh=1
        fi
    fi

    if (( ! _shiki_fresh )); then
        command "${{SHIKI_TERM_BIN:-{bin}}}" _emit --if-stale >/dev/null 2>&1 || true
        if [[ -r "$_shiki_gen_file" ]]; then
            IFS=' ' read -r _shiki_gen _shiki_until < "$_shiki_gen_file" 2>/dev/null || true
        fi
    fi

    if [[ -n "$_shiki_gen" && "$_shiki_gen" != "$__SHIKI_GEN" ]]; then
        __SHIKI_GEN="$_shiki_gen"
        if [[ -s "$_shiki_seq_file" ]]; then
            command cat "$_shiki_seq_file" 2>/dev/null || true
        fi
    fi

    return $_shiki_status
}}

typeset -ga precmd_functions
if (( ! ${{precmd_functions[(Ie)__shiki_precmd]}} )); then
    precmd_functions+=(__shiki_precmd)
fi
"##
        ),
        HookShell::Bash => format!(
            r##"# {display} prompt hook for bash (`eval "$({bin} hook bash)"`)
__SHIKI_GEN="${{__SHIKI_GEN:-}}"

__shiki_prompt_command() {{
    local _shiki_status=$?
    [ -t 1 ] || return $_shiki_status

    local _shiki_rt="" _shiki_cfg=""
    if [ -n "${{SHIKI_HOME:-}}" ]; then
        _shiki_rt="$SHIKI_HOME"
        _shiki_cfg="$SHIKI_HOME/config.toml"
    else
        case "${{OSTYPE:-}}" in
            darwin*)
                _shiki_rt="${{HOME:-}}/Library/Application Support/{display}"
                _shiki_cfg="$_shiki_rt/config.toml"
                ;;
            *)
                local _shiki_cfg_dir="${{XDG_CONFIG_HOME:-${{HOME:-}}/.config}}/{bin}"
                local _shiki_state_dir="${{XDG_STATE_HOME:-${{HOME:-}}/.local/state}}/{bin}"
                if [ -n "${{XDG_RUNTIME_DIR:-}}" ]; then
                    _shiki_rt="$XDG_RUNTIME_DIR/{bin}"
                else
                    _shiki_rt="$_shiki_state_dir"
                fi
                _shiki_cfg="$_shiki_cfg_dir/config.toml"
                ;;
        esac
    fi

    local _shiki_gen_file="$_shiki_rt/current.gen"
    local _shiki_seq_file="$_shiki_rt/current.seq"
    local _shiki_gen="" _shiki_until=0 _shiki_fresh=0

    if [ -r "$_shiki_gen_file" ] && [ -e "$_shiki_seq_file" ] && [ ! "$_shiki_cfg" -nt "$_shiki_gen_file" ]; then
        IFS=' ' read -r _shiki_gen _shiki_until < "$_shiki_gen_file" 2>/dev/null || true
        case "${{_shiki_until:-}}" in
            ''|*[!0-9]*) ;;
            *)
                if [ -n "${{EPOCHSECONDS:-}}" ] && [ "$EPOCHSECONDS" -lt "$_shiki_until" ]; then
                    _shiki_fresh=1
                fi
                ;;
        esac
    fi

    if [ "$_shiki_fresh" -eq 0 ]; then
        command "${{SHIKI_TERM_BIN:-{bin}}}" _emit --if-stale >/dev/null 2>&1 || true
        if [ -r "$_shiki_gen_file" ]; then
            IFS=' ' read -r _shiki_gen _shiki_until < "$_shiki_gen_file" 2>/dev/null || true
        fi
    fi

    if [ -n "$_shiki_gen" ] && [ "$_shiki_gen" != "$__SHIKI_GEN" ]; then
        __SHIKI_GEN="$_shiki_gen"
        if [ -s "$_shiki_seq_file" ]; then
            command cat "$_shiki_seq_file" 2>/dev/null || true
        fi
    fi

    return $_shiki_status
}}

if [[ "$(declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a"* ]]; then
    if [[ " ${{PROMPT_COMMAND[*]-}} " != *" __shiki_prompt_command "* ]]; then
        PROMPT_COMMAND=(__shiki_prompt_command "${{PROMPT_COMMAND[@]}}")
    fi
else
    case ";${{PROMPT_COMMAND:-}};" in
        *";__shiki_prompt_command;"*) ;;
        *)
            PROMPT_COMMAND="__shiki_prompt_command${{PROMPT_COMMAND:+;$PROMPT_COMMAND}}"
            ;;
    esac
fi
"##
        ),
        HookShell::Fish => format!(
            r##"# {display} prompt hook for fish (`{bin} hook fish | source`)
if not set -q __SHIKI_GEN
    set -g __SHIKI_GEN ""
end
if not set -q __SHIKI_OS
    set -g __SHIKI_OS (uname -s 2>/dev/null)
end

function __shiki_fish_prompt --on-event fish_prompt
    set -l _shiki_status $status
    if not isatty stdout
        return $_shiki_status
    end

    set -l _shiki_rt ""
    if set -q SHIKI_HOME; and test -n "$SHIKI_HOME"
        set _shiki_rt "$SHIKI_HOME"
    else if test "$__SHIKI_OS" = Darwin
        set _shiki_rt "$HOME/Library/Application Support/{display}"
    else
        set -l _shiki_state_base "$HOME/.local/state"
        if set -q XDG_STATE_HOME; and test -n "$XDG_STATE_HOME"
            set _shiki_state_base "$XDG_STATE_HOME"
        end
        if set -q XDG_RUNTIME_DIR; and test -n "$XDG_RUNTIME_DIR"
            set _shiki_rt "$XDG_RUNTIME_DIR/{bin}"
        else
            set _shiki_rt "$_shiki_state_base/{bin}"
        end
    end

    set -l _shiki_bin "{bin}"
    if set -q SHIKI_TERM_BIN; and test -n "$SHIKI_TERM_BIN"
        set _shiki_bin "$SHIKI_TERM_BIN"
    end

    command "$_shiki_bin" _emit --if-stale >/dev/null 2>&1

    set -l _shiki_gen_file "$_shiki_rt/current.gen"
    set -l _shiki_seq_file "$_shiki_rt/current.seq"
    if test -r "$_shiki_gen_file"
        set -l _shiki_gen ""
        set -l _shiki_until ""
        read -l _shiki_gen _shiki_until < "$_shiki_gen_file" 2>/dev/null
        if test -n "$_shiki_gen"; and test "$_shiki_gen" != "$__SHIKI_GEN"
            set -g __SHIKI_GEN "$_shiki_gen"
            if test -s "$_shiki_seq_file"
                command cat "$_shiki_seq_file" 2>/dev/null
            end
        end
    end

    return $_shiki_status
end
"##
        ),
    }
}

fn start_marker() -> String {
    format!("# >>> {} initialize >>>", product::CLI_BIN)
}

fn end_marker() -> String {
    format!("# <<< {} initialize <<<", product::CLI_BIN)
}

/// Returns the sibling backup path `<rc>.<cli-bin>.bak` created by `hook install --write-rc`.
#[must_use]
pub fn backup_path_for(rc_path: &Path) -> PathBuf {
    let base = rc_path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "rc".to_string());
    rc_path.with_file_name(format!("{base}.{}.bak", product::CLI_BIN))
}

fn render_rc_block(
    shell: HookShell,
    nl: &str,
    trim_leading_nl: bool,
    remove_if_empty: bool,
) -> String {
    let bin = product::CLI_BIN;
    let start = start_marker();
    let end = end_marker();

    let mut lines = vec![start];
    if trim_leading_nl {
        lines.push(TAG_TRIM_LEADING_NL.to_string());
    }
    if remove_if_empty {
        lines.push(TAG_REMOVE_IF_EMPTY.to_string());
    }

    match shell {
        HookShell::Zsh | HookShell::Bash => {
            let sh_name = shell.as_str();
            lines.push(format!("if command -v {bin} >/dev/null 2>&1; then"));
            lines.push(format!("    eval \"$({bin} hook {sh_name})\""));
            lines.push("fi".to_string());
        }
        HookShell::Fish => {
            lines.push(format!("if command -sq {bin}"));
            lines.push(format!("    {bin} hook fish | source"));
            lines.push("end".to_string());
        }
    }
    lines.push(end);

    let mut out = lines.join(nl);
    out.push_str(nl);
    out
}

/// Renders the human-facing snippet shown when `hook install` is invoked without `--write-rc`.
#[must_use]
pub fn preview_rc_block(shell: HookShell) -> String {
    render_rc_block(shell, "\n", false, false)
}

struct LocatedBlock {
    start_idx: usize,
    block_end: usize,
    trim_leading_nl: bool,
    remove_if_empty: bool,
}

fn find_marked_block(contents: &str) -> Result<Option<LocatedBlock>, String> {
    let start = start_marker();
    let end = end_marker();

    let Some(start_idx) = contents.find(&start) else {
        if contents.contains(&end) {
            return Err(format!(
                "malformed {} rc block: found closing marker without opening marker",
                product::DISPLAY_NAME
            ));
        }
        return Ok(None);
    };

    if start_idx > 0 && contents.as_bytes()[start_idx - 1] != b'\n' {
        return Err(format!(
            "malformed {} rc block: opening marker is not at start of line",
            product::DISPLAY_NAME
        ));
    }

    let search_from = start_idx + start.len();
    let Some(rel_end) = contents[search_from..].find(&end) else {
        return Err(format!(
            "malformed {} rc block: found opening marker without closing marker",
            product::DISPLAY_NAME
        ));
    };
    let end_idx = search_from + rel_end;
    let after_end = end_idx + end.len();

    let block_end = if contents[after_end..].starts_with("\r\n") {
        after_end + 2
    } else if contents[after_end..].starts_with('\n') {
        after_end + 1
    } else {
        after_end
    };

    let slice = &contents[start_idx..block_end];
    Ok(Some(LocatedBlock {
        start_idx,
        block_end,
        trim_leading_nl: slice.contains(TAG_TRIM_LEADING_NL),
        remove_if_empty: slice.contains(TAG_REMOVE_IF_EMPTY),
    }))
}

fn atomic_write_rc(
    target: &Path,
    bytes: &[u8],
    existing_perms: Option<fs::Permissions>,
) -> Result<(), String> {
    let parent = target
        .parent()
        .ok_or_else(|| format!("rc path '{}' has no parent directory", target.display()))?;
    let base_name = target.file_name().and_then(|n| n.to_str()).unwrap_or("rc");

    fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;

    let pid = std::process::id();
    let (mut file, tmp_path) = loop {
        let seq = RC_TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(".{base_name}.tmp.{pid}.{seq}"));
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o644);
        }
        match opts.open(&candidate) {
            Ok(f) => break (f, candidate),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(format!(
                    "open temp file {}: {e_msg}",
                    candidate.display(),
                    e_msg = err
                ))
            }
        }
    };

    let write_res = (|| -> io::Result<()> {
        if let Some(perms) = existing_perms {
            file.set_permissions(perms)?;
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    })();
    drop(file);

    if let Err(err) = write_res {
        let _ = fs::remove_file(&tmp_path);
        return Err(format!("write {}: {err}", target.display()));
    }

    if let Err(err) = fs::rename(&tmp_path, target) {
        let _ = fs::remove_file(&tmp_path);
        return Err(format!("rename into {}: {err}", target.display()));
    }

    #[cfg(unix)]
    if let Ok(dir) = fs::File::open(parent) {
        let _ = dir.sync_all();
    }

    Ok(())
}

/// Atomically installs (or idempotently updates) the marked hook block in `rc_path`, writing a
/// backup file `<rc>.<cli-bin>.bak` when modifying a pre-existing rc file (`PLAN.md` §0 #6 & §10 T08).
pub fn install_hook_to_rc(shell: HookShell, rc_path: &Path) -> Result<String, String> {
    let (existing_opt, existing_perms) = match fs::metadata(rc_path) {
        Ok(meta) => {
            let text = fs::read_to_string(rc_path)
                .map_err(|e| format!("failed to read {}: {e}", rc_path.display()))?;
            (Some(text), Some(meta.permissions()))
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => (None, None),
        Err(err) => return Err(format!("failed to stat {}: {err}", rc_path.display())),
    };

    let nl = match existing_opt.as_deref() {
        Some(text) if text.contains("\r\n") => "\r\n",
        _ => "\n",
    };

    let updated = match existing_opt.as_deref() {
        None => render_rc_block(shell, nl, false, true),
        Some(existing) => {
            if let Some(loc) = find_marked_block(existing)? {
                let replacement =
                    render_rc_block(shell, nl, loc.trim_leading_nl, loc.remove_if_empty);
                let mut buf = String::with_capacity(existing.len() + replacement.len());
                buf.push_str(&existing[..loc.start_idx]);
                buf.push_str(&replacement);
                buf.push_str(&existing[loc.block_end..]);
                buf
            } else {
                // Save the pre-Shiki backup before inserting our marked block for the first time.
                let bak_path = backup_path_for(rc_path);
                if !bak_path.exists() {
                    atomic_write_rc(&bak_path, existing.as_bytes(), existing_perms.clone())?;
                }

                let needs_leading_nl = !existing.is_empty() && !existing.ends_with('\n');
                let block = render_rc_block(shell, nl, needs_leading_nl, false);
                let mut buf = String::with_capacity(existing.len() + nl.len() + block.len());
                buf.push_str(existing);
                if needs_leading_nl {
                    buf.push_str(nl);
                }
                buf.push_str(&block);
                buf
            }
        }
    };

    if existing_opt.as_deref() != Some(updated.as_str()) {
        atomic_write_rc(rc_path, updated.as_bytes(), existing_perms)?;
    }

    Ok(format!(
        "installed {shell} prompt hook in {}\n",
        rc_path.display()
    ))
}

/// Surgically removes the marked hook block from `rc_path`, restoring the pre-install rc file
/// byte-for-byte while preserving any edits outside the block and removing `.bak` residue.
pub fn uninstall_hook_from_rc(shell: HookShell, rc_path: &Path) -> Result<String, String> {
    let bak_path = backup_path_for(rc_path);

    let (existing, existing_perms) = match fs::metadata(rc_path) {
        Ok(meta) => {
            let text = fs::read_to_string(rc_path)
                .map_err(|e| format!("failed to read {}: {e}", rc_path.display()))?;
            (text, meta.permissions())
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            let _ = fs::remove_file(&bak_path);
            return Ok(format!(
                "no {shell} prompt hook found in {}\n",
                rc_path.display()
            ));
        }
        Err(err) => return Err(format!("failed to stat {}: {err}", rc_path.display())),
    };

    let Some(loc) = find_marked_block(&existing)? else {
        let _ = fs::remove_file(&bak_path);
        return Ok(format!(
            "no {shell} prompt hook found in {}\n",
            rc_path.display()
        ));
    };

    let block_start = if loc.trim_leading_nl {
        if loc.start_idx >= 2 && &existing[loc.start_idx - 2..loc.start_idx] == "\r\n" {
            loc.start_idx - 2
        } else if loc.start_idx >= 1 && &existing[loc.start_idx - 1..loc.start_idx] == "\n" {
            loc.start_idx - 1
        } else {
            loc.start_idx
        }
    } else {
        loc.start_idx
    };

    let mut restored = String::with_capacity(existing.len());
    restored.push_str(&existing[..block_start]);
    restored.push_str(&existing[loc.block_end..]);

    if loc.remove_if_empty && restored.is_empty() {
        fs::remove_file(rc_path)
            .map_err(|e| format!("failed to remove {}: {e}", rc_path.display()))?;
    } else {
        atomic_write_rc(rc_path, restored.as_bytes(), Some(existing_perms))?;
    }

    let _ = fs::remove_file(&bak_path);

    Ok(format!(
        "uninstalled {shell} prompt hook from {}\n",
        rc_path.display()
    ))
}
