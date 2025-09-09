#![forbid(unsafe_code)]

use crate::{OrkaGuiApp, UiUpdate};
use tracing::info;
use orka_api::api_ops;

impl OrkaGuiApp {
    pub(crate) fn stop_exec_task(&mut self) {
        if let Some(cancel) = self.exec.cancel.take() { cancel.cancel(); }
        if let Some(task) = self.exec.task.take() { task.abort(); }
        self.exec.running = false;
        self.exec.input = None;
        self.exec.resize = None;
    }

    pub(crate) fn start_exec_task(&mut self) {
        // Stop previous if any
        self.stop_exec_task();
        self.exec.backlog.clear();
        self.exec.dropped = 0;
        self.exec.recv = 0;
        // Resolve selection
        let Some((ns, pod)) = self.current_pod_selection() else { self.last_error = Some("exec: select a Pod first".into()); return; };
        let container = self.exec.container.clone();
        let pty = self.exec.pty;
        let cmd_text = if self.exec.cmd.trim().is_empty() { "/bin/sh".to_string() } else { self.exec.cmd.clone() };
        // naive split by whitespace; quoted args TBD
        let cmd: Vec<String> = cmd_text.split_whitespace().map(|s| s.to_string()).collect();
        if cmd.is_empty() { self.last_error = Some("exec: command is empty".into()); return; }
        let api = self.api.clone();
        let tx_opt = self.watch.updates_tx.clone();
        self.exec.running = true;
        info!(ns = %ns, pod = %pod, container = ?container, pty, cmd = ?cmd, "exec: start requested");
        let task = tokio::spawn(async move {
            let ops = api_ops(api.as_ref());
            match ops.exec_stream(Some(&ns), &pod, container.as_deref(), &cmd, pty).await {
                Ok(mut h) => {
                    // Notify UI with handle pieces
                    if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::ExecStarted { cancel: h.cancel, input: h.input.clone(), resize: h.resize.clone() }); }
                    // Bridge output
                    while let Some(chunk) = h.rx.recv().await {
                        let s = String::from_utf8_lossy(&chunk.bytes).into_owned();
                        if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::ExecData(s)); }
                    }
                    if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::ExecEnded); }
                }
                Err(e) => { if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::ExecError(e.to_string())); } }
            }
        });
        self.exec.task = Some(task);
    }

    /// Open an external terminal (Alacritty preferred) running `kubectl exec -it` to the selected Pod/container.
    pub(crate) fn open_external_exec(&mut self) {
        // Resolve selection
        let Some((ns, pod)) = self.current_pod_selection() else { self.last_error = Some("exec: select a Pod first".into()); return; };
        let container = self.exec.container.clone();
        let mut args: Vec<String> = Vec::new();
        // Prefer to pass current context if known
        if let Some(ctx) = &self.current_context { args.extend(["--context".to_string(), ctx.clone()]); }
        args.extend(["-n".to_string(), ns.clone(), "exec".to_string(), "-it".to_string(), pod.clone()]);
        if let Some(c) = container.as_deref() { if !c.is_empty() { args.extend(["-c".to_string(), c.to_string()]); } }
        // Default command
        let cmd = if self.exec.cmd.trim().is_empty() { "/bin/sh".to_string() } else { self.exec.cmd.clone() };
        args.push("--".to_string());
        args.extend(cmd.split_whitespace().map(|s| s.to_string()));
        let chosen = self.exec.external_cmd.trim().to_string();
        let mut launched = false;
        #[cfg(target_os = "macos")]
        {
            use std::path::Path;
            let is_app_bundle = chosen.ends_with(".app") || chosen.contains(".app/");
            let ghostty_like = chosen.eq_ignore_ascii_case("ghostty") || chosen.to_ascii_lowercase().contains("ghostty");
            let cmd_str = args_to_cmd("kubectl", &args);
            let window_title = format!("exec: {}/{}", ns, pod);
            // Build a portable title-set command using OSC 0
            let title_cmd = format!(
                "printf '\\033]0;%s\\007' {}",
                shell_escape::escape(window_title.as_str().into())
            );
            if chosen.eq_ignore_ascii_case("iterm") || chosen.eq_ignore_ascii_case("iterm2") {
                // iTerm/iTerm2: new window and run two lines with newlines to ensure execution
                let app1 = "iTerm2";
                let app2 = "iTerm";
                let lines_it2 = vec![
                    format!("tell application \"{}\" to create window with default profile", app1),
                    "delay 0.1".to_string(),
                    format!(
                        "tell application \"{}\" to tell current session of current window to write text \"{}\" newline yes",
                        app1,
                        applescript_escape(&title_cmd)
                    ),
                    format!(
                        "tell application \"{}\" to tell current session of current window to write text \"{}\" newline yes",
                        app1,
                        applescript_escape(&cmd_str)
                    ),
                ];
                launched = run_osascript(&lines_it2);
                if !launched {
                    let lines_it = vec![
                        format!("tell application \"{}\" to create window with default profile", app2),
                        "delay 0.1".to_string(),
                        format!(
                            "tell application \"{}\" to tell current session of current window to write text \"{}\" newline yes",
                            app2,
                            applescript_escape(&title_cmd)
                        ),
                        format!(
                            "tell application \"{}\" to tell current session of current window to write text \"{}\" newline yes",
                            app2,
                            applescript_escape(&cmd_str)
                        ),
                    ];
                    launched = run_osascript(&lines_it);
                }
            } else if chosen.eq_ignore_ascii_case("terminal") {
                // Terminal: two do script calls to set title and run exec
                let lines = vec![
                    format!("tell application \"Terminal\" to do script \"{}\"", applescript_escape(&title_cmd)),
                    format!("tell application \"Terminal\" to do script \"{}\"", applescript_escape(&cmd_str)),
                ];
                launched = run_osascript(&lines);
            } else if ghostty_like {
                // Prefer Ghostty new window; try several invocation forms
                // 1) ghostty CLI
                // Set title first if shell available
                let ghostty_title = format!(
                    "--new-window --execute /bin/sh -- -lc {}",
                    shell_escape::escape(format!("{}; {}", title_cmd, cmd_str).into())
                );
                launched = std::process::Command::new("ghostty")
                    .args(ghostty_title.split_whitespace())
                    .spawn().is_ok()
                    // 2) ghostty CLI with -- separator
                    || std::process::Command::new("ghostty")
                        .args(["--new-window", "--", "kubectl"]).args(&args)
                        .spawn().is_ok()
                    // 3) open -n -a Ghostty with args (forces new instance/window)
                    || std::process::Command::new("open")
                        .args(["-n", "-a", "Ghostty", "--args", "--new-window", "--execute", "/bin/sh", "--", "-lc"]) 
                        .arg(format!("{}; {}", title_cmd, cmd_str))
                        .spawn().is_ok()
                    // 4) open -n -a Ghostty with -- separator
                    || std::process::Command::new("open")
                        .args(["-n", "-a", "Ghostty", "--args", "--", "kubectl"]).args(&args)
                        .spawn().is_ok();
            } else if chosen.eq_ignore_ascii_case("alacritty") {
                launched = std::process::Command::new("alacritty").args(["-e", "kubectl"]).args(&args).spawn().is_ok();
            } else if is_app_bundle {
                // Explicit app bundle path
                let app_path = Path::new(&chosen);
                let mut cmd = std::process::Command::new("open");
                cmd.arg("-n").arg("-a").arg(app_path).args(["--args", "--", "kubectl"]).args(&args);
                launched = cmd.spawn().is_ok();
            } else {
                // Generic open -n -a AppName --args -- kubectl ... (new instance → new window)
                let mut cmd = std::process::Command::new("open");
                cmd.args(["-n", "-a", &chosen, "--args", "--", "kubectl"]).args(&args);
                launched = cmd.spawn().is_ok();
            }
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let mut cmd = std::process::Command::new(&chosen);
            // Try conventional -e behavior; if it fails, try passing command as args
            launched = cmd.args(["-e", "kubectl"]).args(&args).spawn().is_ok()
                || std::process::Command::new(&chosen).arg("--").arg("kubectl").args(&args).spawn().is_ok();
        }
        #[cfg(target_os = "windows")]
        {
            // Use Windows Terminal if chosen, else fallbacks
            if chosen.eq_ignore_ascii_case("wt.exe") {
                launched = std::process::Command::new("wt.exe").args(["nt", "kubectl"]).args(&args).spawn().is_ok();
            } else {
                launched = std::process::Command::new(&chosen).arg("kubectl").args(&args).spawn().is_ok();
            }
        }
        if !launched { self.last_error = Some(format!("exec: failed to launch external terminal: {}", chosen)); }
    }

    /// Run a one-shot command via kubectl exec, capturing output (stdout+stderr) and streaming lines into the UI.
    pub(crate) fn start_exec_oneshot_task(&mut self) {
        // Stop previous if any
        if let Some(task) = self.exec.task.take() { task.abort(); }
        self.exec.backlog.clear();
        self.exec.dropped = 0;
        self.exec.recv = 0;
        let Some((ns, pod)) = self.current_pod_selection() else { self.last_error = Some("exec: select a Pod first".into()); return; };
        let container = self.exec.container.clone();
        let cmd_text = if self.exec.cmd.trim().is_empty() { "/bin/sh -c 'echo shell missing'".to_string() } else { self.exec.cmd.clone() };
        let ctx_opt = self.current_context.clone();
        let tx_opt = self.watch.updates_tx.clone();
        self.exec.running = true;
        let task = tokio::spawn(async move {
            use std::io::{BufRead, BufReader};
            use std::process::{Command, Stdio};
            let mut base = Command::new("kubectl");
            if let Some(ctx) = ctx_opt.as_ref() { base.arg("--context").arg(ctx); }
            base.arg("-n").arg(&ns).arg("exec").arg(&pod);
            if let Some(c) = container.as_deref() { if !c.is_empty() { base.arg("-c").arg(c); } }
            // non-tty, stdin closed
            base.arg("--");
            // Use sh -lc 'cmd' to support pipelines/quotes
            base.arg("/bin/sh").arg("-lc").arg(&cmd_text);
            base.stdout(Stdio::piped()).stderr(Stdio::piped());
            let mut child = match base.spawn() {
                Ok(c) => c,
                Err(e) => { if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::ExecError(format!("exec: spawn: {}", e))); } return; }
            };
            let mut joins: Vec<std::thread::JoinHandle<()>> = Vec::new();
            if let Some(out) = child.stdout.take() {
                let tx2 = tx_opt.clone();
                joins.push(std::thread::spawn(move || {
                    let reader = BufReader::new(out);
                    for line in reader.lines().flatten() { if let Some(tx) = &tx2 { let _ = tx.send(UiUpdate::ExecData(line)); } }
                }));
            }
            if let Some(err) = child.stderr.take() {
                let tx2 = tx_opt.clone();
                joins.push(std::thread::spawn(move || {
                    let reader = BufReader::new(err);
                    for line in reader.lines().flatten() { if let Some(tx) = &tx2 { let _ = tx.send(UiUpdate::ExecData(line)); } }
                }));
            }
            let _ = child.wait();
            for j in joins { let _ = j.join(); }
            if let Some(tx) = &tx_opt { let _ = tx.send(UiUpdate::ExecEnded); }
        });
        self.exec.task = Some(task);
    }
}

fn args_to_cmd(bin: &str, args: &[String]) -> String {
    let mut s = String::new();
    s.push_str(bin);
    for a in args { s.push(' '); s.push_str(&shell_escape::escape(a.as_str().into())); }
    s
}

#[cfg(target_os = "macos")]
fn applescript_escape(s: &str) -> String {
    // Escape characters significant to AppleScript double-quoted strings
    // Replace backslash first to avoid double-escaping
    let s = s.replace('\\', "\\\\");
    let s = s.replace('"', "\\\"");
    s.replace('\n', "\\n")
}

#[cfg(target_os = "macos")]
fn run_osascript(lines: &[String]) -> bool {
    let mut cmd = std::process::Command::new("osascript");
    for l in lines { cmd.arg("-e").arg(l); }
    cmd.spawn().is_ok()
}
