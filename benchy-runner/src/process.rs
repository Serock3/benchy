//! Owned local/SSH processes: readiness on either stream, bounded shutdown and capture.
//! Kill only the PID reported by this invocation, never a name or an existing port owner.
use anyhow::{Context, Result, bail, ensure};
use std::{
    ffi::{OsStr, OsString},
    net::SocketAddr,
    process::{Output, Stdio},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    sync::mpsc,
    task::JoinHandle,
    time::{sleep, timeout},
};

const PID_MARKER: &str = "BENCHY_PROCESS_PID ";
const READY_TIMEOUT: Duration = Duration::from_secs(20);
const STOP_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_CAPTURE: usize = 16 * 1024 * 1024;

pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn remote_command(program: &str, args: &[String]) -> String {
    std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .map(shell_quote)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Always attempt every shutdown, even when one peer is unreachable.
pub async fn stop_all(processes: &mut [Process]) -> Result<()> {
    let mut errors = Vec::new();
    for process in processes.iter_mut().rev() {
        if let Err(error) = process.stop().await {
            errors.push(format!("{error:#}"));
        }
    }
    ensure!(
        errors.is_empty(),
        "process cleanup failed: {}",
        errors.join("; ")
    );
    Ok(())
}

pub fn with_cleanup<T>(result: Result<T>, cleanup: Result<()>) -> Result<T> {
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => {
            Err(error).context(format!("cleanup also failed: {cleanup:#}"))
        }
    }
}

pub struct ProcessBuilder {
    program: OsString,
    args: Vec<OsString>,
    peer: Option<String>,
    sudo: bool,
    ready: Option<String>,
    capture: bool,
}

pub struct Process {
    child: Child,
    pid: Option<u32>,
    peer: Option<String>,
    sudo: bool,
    stdout: Option<JoinHandle<Result<Vec<u8>>>>,
    stderr: Option<JoinHandle<Result<Vec<u8>>>>,
}

impl ProcessBuilder {
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|arg| arg.as_ref().to_owned()));
        self
    }
    pub fn on_peer(mut self, peer: &str) -> Self {
        self.peer = Some(peer.to_owned());
        self
    }
    pub fn sudo(mut self) -> Self {
        self.sudo = true;
        self
    }
    pub fn ready_when(mut self, marker: &str) -> Self {
        self.ready = Some(marker.to_owned());
        self
    }
    pub fn capture_stdout(mut self) -> Self {
        self.capture = true;
        self
    }

    pub async fn spawn(self) -> Result<Process> {
        let mut command = if let Some(peer) = &self.peer {
            let values = self
                .args
                .iter()
                .map(|arg| {
                    arg.to_str()
                        .context("SSH arguments must be UTF-8")
                        .map(str::to_owned)
                })
                .collect::<Result<Vec<_>>>()?;
            let inner = format!(
                "echo {PID_MARKER}$$; exec {}",
                remote_command(
                    self.program.to_str().context("SSH program must be UTF-8")?,
                    &values
                )
            );
            let remote = format!(
                "{}sh -c {}",
                if self.sudo { "sudo -n " } else { "" },
                shell_quote(&inner)
            );
            let mut command = Command::new("ssh");
            command.args([
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                peer,
                &remote,
            ]);
            command
        } else {
            let mut command = Command::new(if self.sudo { "sudo" } else { "sh" });
            if self.sudo {
                command.args(["-n", "sh"]);
            }
            command
                .args([
                    "-c",
                    "echo BENCHY_PROCESS_PID $$; exec \"$@\"",
                    "benchy-process",
                ])
                .arg(&self.program)
                .args(&self.args);
            command
        };
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("failed to start {:?}", self.program))?;
        let mut process = Process {
            child,
            pid: None,
            peer: self.peer,
            sudo: self.sudo,
            stdout: None,
            stderr: None,
        };
        let result = async {
            let (tx, mut rx) = mpsc::unbounded_channel();
            let stderr = process.child.stderr.take().context("missing stderr")?;
            process.stderr = Some(read_stream(
                BufReader::new(stderr),
                self.ready.clone(),
                tx.clone(),
                false,
            ));
            let mut stdout = BufReader::new(process.child.stdout.take().context("missing stdout")?);
            let mut line = String::new();
            timeout(READY_TIMEOUT, stdout.read_line(&mut line))
                .await
                .context("timed out reading process PID")??;
            let pid: u32 = line
                .trim()
                .strip_prefix(PID_MARKER)
                .context("missing process PID marker")?
                .parse()?;
            ensure!(pid > 1, "invalid owned PID");
            process.pid = Some(pid);
            process.stdout = Some(read_stream(stdout, self.ready.clone(), tx, self.capture));
            if self.ready.is_some() {
                timeout(READY_TIMEOUT, async {
                    while let Some(event) = rx.recv().await {
                        if event? {
                            return Ok(());
                        }
                    }
                    bail!("process exited before reporting readiness")
                })
                .await
                .context("process readiness timed out")??;
                process.check_alive()?;
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            if let Err(cleanup) = process.stop().await {
                return Err(error).context(format!("startup cleanup also failed: {cleanup:#}"));
            }
            return Err(error);
        }
        Ok(process)
    }
}

impl Process {
    pub fn command(program: impl AsRef<OsStr>) -> ProcessBuilder {
        ProcessBuilder {
            program: program.as_ref().to_owned(),
            args: Vec::new(),
            peer: None,
            sudo: false,
            ready: None,
            capture: false,
        }
    }
    pub fn pid(&self) -> u32 {
        self.pid.expect("spawned processes have a PID")
    }

    pub fn check_alive(&mut self) -> Result<()> {
        if let Some(status) = self.child.try_wait()? {
            bail!("owned process exited: {status}");
        }
        Ok(())
    }

    /// Check the exact listener owner, without opening an iperf control connection.
    pub async fn wait_tcp(&mut self, address: SocketAddr) -> Result<()> {
        for _ in 0..20 {
            self.check_alive()?;
            let args = [
                "-H".to_owned(),
                "-ltnp".to_owned(),
                format!("sport = :{}", address.port()),
            ];
            let output = match &self.peer {
                Some(peer) => {
                    crate::checked_output("ssh", [peer, &remote_command("ss", &args)]).await?
                }
                None => crate::checked_output("ss", &args).await?,
            };
            if owns_listener(
                &String::from_utf8_lossy(&output.stdout),
                address,
                self.pid(),
            ) {
                return Ok(());
            }
            sleep(Duration::from_millis(100)).await;
        }
        bail!("owned process did not listen on {address}; refusing another listener")
    }

    pub async fn wait_output(&mut self) -> Result<Output> {
        let status = self.child.wait().await?;
        let stdout = collect(&mut self.stdout).await?;
        let stderr = collect(&mut self.stderr).await?;
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    }

    async fn signal(&mut self, signal: &str) -> Result<()> {
        let Some(pid) = self.pid else {
            return self.child.kill().await.map_err(Into::into);
        };
        let mut args = vec![signal.to_owned(), pid.to_string()];
        let program = if self.sudo {
            args.splice(0..0, ["-n".to_owned(), "kill".to_owned()]);
            "sudo"
        } else {
            "kill"
        };
        let output = match &self.peer {
            Some(peer) => {
                crate::checked_output("ssh", [peer, &remote_command(program, &args)]).await
            }
            None => crate::checked_output(program, &args).await,
        };
        // The program may exit between try_wait and kill. An SSH failure while still
        // running is an error, rather than an assertion that the remote PID was stopped.
        if output.is_err() && self.child.try_wait()?.is_some() {
            return Ok(());
        }
        output.map(|_| ())
    }

    pub async fn stop(&mut self) -> Result<()> {
        if self.child.try_wait()?.is_none() {
            self.signal("-INT").await?;
            if let Ok(status) = timeout(STOP_TIMEOUT, self.child.wait()).await {
                status?;
            } else {
                self.signal("-KILL").await?;
                self.child.kill().await?;
                bail!("owned process required forced shutdown");
            }
        }
        collect(&mut self.stdout).await?;
        collect(&mut self.stderr).await?;
        Ok(())
    }
}

fn owns_listener(output: &str, address: SocketAddr, pid: u32) -> bool {
    output.lines().any(|line| {
        line.split_whitespace()
            .any(|field| field == address.to_string())
            && line.contains(&format!("pid={pid},"))
    })
}

fn read_stream<R: tokio::io::AsyncBufRead + Unpin + Send + 'static>(
    mut stream: R,
    marker: Option<String>,
    ready: mpsc::UnboundedSender<Result<bool>>,
    capture: bool,
) -> JoinHandle<Result<Vec<u8>>> {
    tokio::spawn(async move {
        let mut output = Vec::new();
        let mut line = String::new();
        loop {
            line.clear();
            let read = stream.read_line(&mut line).await;
            match read {
                Ok(0) => break,
                Ok(_) => {
                    if marker.as_ref().is_some_and(|marker| line.contains(marker)) {
                        let _ = ready.send(Ok(true));
                    }
                    if capture {
                        ensure!(
                            output.len() + line.len() <= MAX_CAPTURE,
                            "process output exceeds capture limit"
                        );
                        output.extend_from_slice(line.as_bytes());
                    } else {
                        eprint!("{line}");
                    }
                }
                Err(error) => {
                    let _ = ready.send(Err(anyhow::anyhow!("process output failed: {error}")));
                    return Err(error.into());
                }
            }
        }
        Ok(output)
    })
}

async fn collect(task: &mut Option<JoinHandle<Result<Vec<u8>>>>) -> Result<Vec<u8>> {
    let Some(mut task) = task.take() else {
        return Ok(Vec::new());
    };
    match timeout(STOP_TIMEOUT, &mut task).await {
        Ok(result) => result.context("process output task failed")?,
        Err(_) => {
            task.abort();
            bail!("process output remained open after shutdown");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn listener_must_have_the_exact_address_and_pid() {
        let line = "LISTEN 0 4096 10.0.1.2:5201 0.0.0.0:* users:((\"iperf3\",pid=1234,fd=3))";
        assert!(owns_listener(line, "10.0.1.2:5201".parse().unwrap(), 1234));
        assert!(!owns_listener(line, "10.0.1.2:5201".parse().unwrap(), 123));
        assert!(!owns_listener(line, "10.0.1.1:5201".parse().unwrap(), 1234));
    }
    #[tokio::test]
    async fn readiness_on_stderr_then_shutdown_closes_the_owned_process() {
        let mut child = Process::command("sh")
            .args(["-c", "echo Ready >&2; exec sleep 60"])
            .ready_when("Ready")
            .spawn()
            .await
            .unwrap();
        child.check_alive().unwrap();
        child.stop().await.unwrap();
        assert!(child.check_alive().is_err());
    }
    #[tokio::test]
    async fn captured_output_preserves_json_and_quoted_arguments() {
        let mut child = Process::command("sh")
            .args([
                "-c",
                "printf '%s' \"$1\"",
                "test",
                "{\"value\":\"it's safe\"}",
            ])
            .capture_stdout()
            .spawn()
            .await
            .unwrap();
        let output = child.wait_output().await.unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "{\"value\":\"it's safe\"}"
        );
        child.stop().await.unwrap();
    }
    #[tokio::test]
    async fn an_unused_stream_can_close_before_the_readiness_marker() {
        let mut child = Process::command("sh")
            .args(["-c", "exec 2>&-; echo Ready; exec sleep 60"])
            .ready_when("Ready")
            .spawn()
            .await
            .unwrap();
        child.stop().await.unwrap();
    }
    #[test]
    fn remote_arguments_are_literal_shell_arguments() {
        assert_eq!(
            remote_command("a b", &["it's $(safe)".into()]),
            "'a b' 'it'\\''s $(safe)'"
        );
    }
}
