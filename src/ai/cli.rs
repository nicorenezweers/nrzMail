//! KI über ein Programm auf dem Server statt über eine API: **Claude Code** (`claude -p`) und
//! **OpenCode** (`opencode run`). Der Befehl steht in den KI-Einstellungen (Vorgabe `claude`
//! bzw. `opencode`, auch ein voller Pfad mit festen Argumenten). Das Programm läuft ohne Shell,
//! in einem leeren temporären Ordner und mit Zeitlimit; ein eingetragener Schlüssel geht als
//! Umgebungsvariable mit (Claude Code: `ANTHROPIC_API_KEY`, bzw. `CLAUDE_CODE_OAUTH_TOKEN` für
//! Token aus `claude setup-token`), sonst gilt die Anmeldung des Programms selbst.

use super::client::{Completion, Prompt, Role, Turn};
use super::error::{AiError, AiResult};
use super::settings::AiConfig;
use crate::api;
use std::process::Stdio;
use std::sync::LazyLock;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Höchstens so viel Ausgabe wird gelesen.
const MAX_OUTPUT: usize = 1_000_000;

/// Verlauf als ein Text (die Programme bekommen eine einzige Anfrage).
fn transcript(turns: &[Turn]) -> String {
    if turns.len() == 1 {
        return turns[0].content.clone();
    }
    let mut out = String::new();
    for t in turns {
        out.push_str(match t.role {
            Role::User => "### Benutzer\n",
            Role::Assistant => "### Assistent\n",
        });
        out.push_str(t.content.trim());
        out.push_str("\n\n");
    }
    out.push_str("### Assistent\n");
    out
}

/// Programm und feste Argumente aus dem Befehl (Leerzeichen trennen, Anführungszeichen halten zusammen).
fn split_command(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in command.chars() {
        match (c, quote) {
            ('"' | '\'', None) => quote = Some(c),
            (c, Some(q)) if c == q => quote = None,
            (c, None) if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            (c, _) => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Aufruf zusammenstellen: (Programm, Argumente, Eingabe über stdin).
fn invocation(ai: &AiConfig, model: &str, prompt: &Prompt, turns: &[Turn]) -> AiResult<(String, Vec<String>, Option<String>)> {
    let mut parts = split_command(&ai.url);
    if parts.is_empty() {
        return Err(AiError::Unavailable(format!("Für {} fehlt der Befehl.", ai.provider.name)));
    }
    let program = parts.remove(0);
    let mut args = parts;
    let text = transcript(turns);
    match ai.provider.id {
        "claude-code" => {
            args.extend(["-p".into(), "--output-format".into(), "text".into()]);
            args.extend(["--system-prompt".into(), prompt.system.clone()]);
            if !model.trim().is_empty() {
                args.extend(["--model".into(), model.trim().into()]);
            }
            Ok((program, args, Some(text)))
        }
        _ => {
            // OpenCode: `opencode run [--model anbieter/modell] <nachricht>`
            args.push("run".into());
            if !model.trim().is_empty() {
                args.extend(["--model".into(), model.trim().into()]);
            }
            args.push(format!("{}\n\n---\n\n{text}", prompt.system));
            Ok((program, args, None))
        }
    }
}

/// Farb- und Steuercodes aus der Ausgabe entfernen.
fn strip_ansi(s: &str) -> String {
    static ANSI: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07]*\x07").expect("Regex"));
    ANSI.replace_all(s, "").to_string()
}

fn command(ai: &AiConfig, program: &str, args: &[String], dir: &std::path::Path) -> Command {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("NO_COLOR", "1")
        .env("CI", "1");
    if !ai.key.is_empty() && ai.provider.id == "claude-code" {
        if ai.key.starts_with("sk-ant-oat") {
            cmd.env("CLAUDE_CODE_OAUTH_TOKEN", &ai.key);
        } else {
            cmd.env("ANTHROPIC_API_KEY", &ai.key);
        }
    }
    cmd
}

fn spawn_error(ai: &AiConfig, program: &str, e: std::io::Error) -> AiError {
    if e.kind() == std::io::ErrorKind::NotFound {
        AiError::Unavailable(format!(
            "{}: das Programm „{program}“ ist auf dem Server nicht installiert (oder nicht im PATH) – Befehl unter Einstellungen → KI anpassen.",
            ai.provider.name
        ))
    } else {
        AiError::Failed(format!("{}: „{program}“ liess sich nicht starten: {e}", ai.provider.name))
    }
}

/// Anfrage an das Programm.
pub async fn complete(ai: &AiConfig, model: &str, prompt: &Prompt, turns: &[Turn]) -> AiResult<Completion> {
    let (program, args, stdin) = invocation(ai, model, prompt, turns)?;
    let dir = tempfile_dir()?;
    let mut child = command(ai, &program, &args, dir.path())
        .spawn()
        .map_err(|e| spawn_error(ai, &program, e))?;
    if let Some(mut pipe) = child.stdin.take() {
        if let Some(input) = stdin {
            let _ = pipe.write_all(input.as_bytes()).await;
        }
        drop(pipe);
    }
    let out = tokio::time::timeout(prompt.timeout, child.wait_with_output())
        .await
        .map_err(|_| {
            AiError::Unreachable(format!(
                "{} antwortet nicht (Zeitüberschreitung nach {} s).",
                ai.provider.name,
                prompt.timeout.as_secs()
            ))
        })?
        .map_err(|e| AiError::Failed(format!("{}: {e}", ai.provider.name)))?;
    let stdout = strip_ansi(&String::from_utf8_lossy(&out.stdout[..out.stdout.len().min(MAX_OUTPUT)]));
    if !out.status.success() {
        let stderr = strip_ansi(&String::from_utf8_lossy(&out.stderr));
        let msg: String = stderr
            .lines()
            .chain(stdout.lines())
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .last()
            .unwrap_or("ohne Meldung")
            .chars()
            .take(300)
            .collect();
        let lower = msg.to_lowercase();
        if lower.contains("login") || lower.contains("auth") || lower.contains("api key") || lower.contains("credential") {
            return Err(AiError::KeyRejected(format!(
                "{} ist nicht angemeldet: {msg} – auf dem Server einmal anmelden (z. B. `claude setup-token`) oder einen Schlüssel eintragen.",
                ai.provider.name
            )));
        }
        return Err(AiError::Failed(format!("{}: {msg}", ai.provider.name)));
    }
    let text = super::client::strip_thinking(stdout.trim());
    if text.is_empty() {
        return Err(AiError::Failed(format!("{}: leere Antwort.", ai.provider.name)));
    }
    Ok(Completion {
        text,
        model: if model.trim().is_empty() {
            ai.provider.name.to_string()
        } else {
            model.trim().to_string()
        },
    })
}

fn tempfile_dir() -> AiResult<TempDir> {
    let path = std::env::temp_dir().join(format!("nrzmail-ai-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).map_err(|e| AiError::Failed(format!("Temporärer Ordner: {e}")))?;
    Ok(TempDir(path))
}

/// Leerer Arbeitsordner für das Programm, wird danach gelöscht.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Modelle: Claude Code kennt Kurznamen, OpenCode listet seine mit `opencode models`.
pub async fn models(ai: &AiConfig) -> AiResult<Vec<api::AiModel>> {
    let model = |id: &str, name: &str| api::AiModel { id: id.into(), name: name.into(), free: false };
    if ai.provider.id == "claude-code" {
        return Ok(vec![
            model("sonnet", "Claude Sonnet (neuestes)"),
            model("opus", "Claude Opus (neuestes)"),
            model("haiku", "Claude Haiku (neuestes)"),
        ]);
    }
    let mut parts = split_command(&ai.url);
    if parts.is_empty() {
        return Err(AiError::Unavailable(format!("Für {} fehlt der Befehl.", ai.provider.name)));
    }
    let program = parts.remove(0);
    parts.push("models".into());
    let dir = tempfile_dir()?;
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        command(ai, &program, &parts, dir.path()).output(),
    )
    .await
    .map_err(|_| AiError::Unreachable(format!("{} antwortet nicht.", ai.provider.name)))?
    .map_err(|e| spawn_error(ai, &program, e))?;
    let text = strip_ansi(&String::from_utf8_lossy(&out.stdout));
    let mut list: Vec<api::AiModel> = text
        .lines()
        .map(str::trim)
        .filter(|l| l.contains('/') && !l.contains(char::is_whitespace))
        .map(|l| model(l, l))
        .collect();
    list.sort_by(|a, b| a.id.cmp(&b.id));
    list.dedup_by(|a, b| a.id == b.id);
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_and_transcripts() {
        assert_eq!(split_command("claude"), vec!["claude"]);
        assert_eq!(
            split_command("/opt/bin/claude --verbose \"a b\""),
            vec!["/opt/bin/claude", "--verbose", "a b"]
        );
        let t = transcript(&[Turn::user("Hallo"), Turn::assistant("Hi"), Turn::user("Frage")]);
        assert!(t.starts_with("### Benutzer\nHallo") && t.ends_with("### Assistent\n"));
        assert_eq!(transcript(&[Turn::user("nur eins")]), "nur eins");
        assert_eq!(strip_ansi("\x1b[1mfett\x1b[0m"), "fett");
    }
}
