//! Execução do jogo: captura de log, detecção de crash e resumo amigável.
use crate::error::{AppError, Result};
use crate::java::hide_window;
use crate::report::{Report, Reporter};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::SystemTime;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};

const TAIL_LINES: usize = 600;

#[derive(Debug, Clone)]
pub struct ExitInfo {
    pub code: Option<i32>,
    pub crashed: bool,
    pub summary: Option<String>,
    pub crash_report: Option<PathBuf>,
}

/// Alça do jogo em execução.
pub struct GameHandle {
    /// Resolve quando o jogo fecha (o evento GameExit já foi emitido).
    pub task: tokio::task::JoinHandle<ExitInfo>,
    pub kill: Option<oneshot::Sender<()>>,
}
impl GameHandle {
    /// Encerra o jogo à força.
    pub fn kill(&mut self) {
        if let Some(k) = self.kill.take() {
            let _ = k.send(());
        }
    }
}

/// Traduz o log/código de saída em uma mensagem que um leigo entende.
pub fn summarize_crash(lines: &[String], code: Option<i32>) -> String {
    let has = |needle: &str| lines.iter().any(|l| l.contains(needle));
    let code_txt = code.map_or(String::new(), |c| format!(" (código {c})"));
    if has("OutOfMemoryError") {
        "O jogo ficou sem memória. Aumente a RAM em Configurações (ou feche outros programas) e tente de novo.".into()
    } else if has("Could not reserve enough space") || has("Invalid maximum heap size") || has("Could not create the Java Virtual Machine") {
        "Não foi possível reservar tanta RAM para o jogo. Diminua a RAM em Configurações.".into()
    } else if has("UnsupportedClassVersionError") {
        "Versão do Java incompatível. Use \"Reparar instalação\" em Configurações para reinstalar o Java correto.".into()
    } else if has("EXCEPTION_ACCESS_VIOLATION") || has("hs_err_pid") || has("A fatal error has been detected by the Java Runtime") {
        "O Java travou (geralmente driver de vídeo desatualizado ou antivírus). Atualize o driver da placa de vídeo e tente de novo.".into()
    } else if has("GLFW error") || has("OpenGL") && has("not supported") || has("does not appear to support OpenGL") {
        "Seu driver de vídeo não suporta o necessário (OpenGL). Atualize o driver da placa de vídeo.".into()
    } else if has("Missing or unsupported mandatory dependencies") || has("Mod loading has failed") || has("Failed to load mod") || has("ModLoadingException") {
        "Um mod falhou ao carregar. Use \"Reparar instalação\" em Configurações; se continuar, copie o log e envie ao administrador.".into()
    } else if has("Address already in use") || has("BindException") {
        "Uma porta de rede já está em uso — talvez o jogo já esteja aberto. Feche o Minecraft e tente de novo.".into()
    } else {
        format!("O jogo fechou inesperadamente{code_txt}. Tente \"Reparar instalação\"; se continuar, copie o log e envie ao administrador.")
    }
}

/// Crash report mais recente criado depois de `since`.
pub fn newest_crash_report(game_dir: &Path, since: SystemTime) -> Option<PathBuf> {
    std::fs::read_dir(game_dir.join("crash-reports"))
        .ok()?
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            let t = m.modified().ok()?;
            (m.is_file() && t >= since).then(|| (t, e.path()))
        })
        .max_by_key(|(t, _)| *t)
        .map(|(_, p)| p)
}

async fn pump<R: tokio::io::AsyncRead + Unpin + Send + 'static>(r: R, stream: &'static str, tx: mpsc::UnboundedSender<(String, &'static str)>) {
    let mut r = BufReader::new(r);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match r.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(&buf).trim_end_matches(['\r', '\n']).to_string();
                if tx.send((line, stream)).is_err() {
                    break;
                }
            }
        }
    }
}

/// Inicia o jogo e monitora em segundo plano. Retorna assim que o processo foi criado.
pub async fn launch(cmd: Vec<String>, game_dir: PathBuf, log_file: PathBuf, rep: Reporter) -> Result<GameHandle> {
    let (exe, args) = cmd.split_first().ok_or_else(|| AppError::new("Comando de execução vazio."))?;
    tokio::fs::create_dir_all(&game_dir).await?;
    if let Some(p) = log_file.parent() {
        let _ = tokio::fs::create_dir_all(p).await;
    }
    let mut c = Command::new(exe);
    c.args(args).current_dir(&game_dir).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(false);
    hide_window(&mut c);
    let started = SystemTime::now();
    let mut child = c
        .spawn()
        .map_err(|e| AppError::new(format!("Não foi possível iniciar o Java ({e}). O antivírus pode ter bloqueado; use \"Reparar instalação\".")))?;

    let (tx, mut rx) = mpsc::unbounded_channel::<(String, &'static str)>();
    tokio::spawn(pump(child.stdout.take().unwrap(), "stdout", tx.clone()));
    tokio::spawn(pump(child.stderr.take().unwrap(), "stderr", tx));
    let (kill_tx, kill_rx) = oneshot::channel::<()>();

    rep(Report::GameStarted);
    let task = tokio::spawn(async move {
        let wait = tokio::spawn(async move {
            let mut killed = false;
            let status = tokio::select! {
                s = child.wait() => s,
                _ = kill_rx => { killed = true; let _ = child.kill().await; child.wait().await }
            };
            (status, killed)
        });

        let mut log = tokio::fs::File::create(&log_file).await.ok();
        let mut tail: VecDeque<String> = VecDeque::with_capacity(TAIL_LINES);
        while let Some((line, stream)) = rx.recv().await {
            if let Some(f) = log.as_mut() {
                let _ = f.write_all(line.as_bytes()).await;
                let _ = f.write_all(b"\n").await;
            }
            if tail.len() == TAIL_LINES {
                tail.pop_front();
            }
            tail.push_back(line.clone());
            rep(Report::Log { line, stream: stream.into() });
        }
        let (status, killed) = wait.await.unwrap_or((Err(std::io::Error::other("falha interna")), false));
        let code = status.ok().and_then(|s| s.code());
        let crashed = !killed && code != Some(0);
        let report = if crashed { newest_crash_report(&game_dir, started) } else { None };
        let mut lines: Vec<String> = tail.into_iter().collect();
        if let Some(p) = &report {
            if let Ok(t) = tokio::fs::read_to_string(p).await {
                lines.extend(t.lines().take(120).map(String::from));
            }
        }
        let summary = crashed.then(|| summarize_crash(&lines, code));
        let info = ExitInfo { code, crashed, summary: summary.clone(), crash_report: report.clone() };
        rep(Report::GameExit { code, crashed, summary, crash_report: report.map(|p| p.to_string_lossy().into_owned()) });
        info
    });
    Ok(GameHandle { task, kill: Some(kill_tx) })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn l(s: &str) -> Vec<String> {
        s.lines().map(String::from).collect()
    }

    #[test]
    fn resumos_de_crash() {
        let c = |t: &str, code| summarize_crash(&l(t), code);
        assert!(c("java.lang.OutOfMemoryError: Java heap space", Some(1)).contains("sem memória"));
        assert!(c("Error occurred during initialization of VM\nCould not reserve enough space for 8388608KB object heap", Some(1)).contains("reservar"));
        assert!(c("java.lang.UnsupportedClassVersionError: x", Some(1)).contains("Java incompatível"));
        assert!(c("# A fatal error has been detected by the Java Runtime Environment:\n# EXCEPTION_ACCESS_VIOLATION", Some(1)).contains("driver de vídeo"));
        assert!(c("net.neoforged.fml.ModLoadingException: Mod loading has failed", Some(1)).contains("mod falhou"));
        let generic = c("nada conhecido", Some(-1073740791));
        assert!(generic.contains("fechou inesperadamente") && generic.contains("-1073740791"));
    }

    #[test]
    fn crash_report_mais_recente() {
        let dir = tempfile::tempdir().unwrap();
        let cr = dir.path().join("crash-reports");
        std::fs::create_dir_all(&cr).unwrap();
        let before = SystemTime::now() - std::time::Duration::from_secs(60);
        std::fs::write(cr.join("crash-a.txt"), "a").unwrap();
        assert!(newest_crash_report(dir.path(), before).is_some());
        // tudo é "antigo" em relação a um instante futuro
        assert!(newest_crash_report(dir.path(), SystemTime::now() + std::time::Duration::from_secs(60)).is_none());
        assert!(newest_crash_report(&dir.path().join("naoexiste"), before).is_none());
    }

    #[tokio::test]
    async fn launch_captura_log_e_detecta_codigo_de_saida() {
        use std::sync::{Arc, Mutex};
        let dir = tempfile::tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::<Report>::new()));
        let e2 = events.clone();
        let rep: Reporter = Arc::new(move |r| e2.lock().unwrap().push(r));
        #[cfg(windows)]
        let cmd = vec!["cmd".to_string(), "/C".into(), "echo OutOfMemoryError simulado & exit 3".into()];
        #[cfg(not(windows))]
        let cmd = vec!["sh".to_string(), "-c".into(), "echo OutOfMemoryError simulado; exit 3".into()];
        let h = launch(cmd, dir.path().to_path_buf(), dir.path().join("logs").join("x.log"), rep).await.unwrap();
        let info = h.task.await.unwrap();
        assert_eq!(info.code, Some(3));
        assert!(info.crashed);
        assert!(info.summary.unwrap().contains("sem memória"));
        let ev = events.lock().unwrap();
        assert!(ev.iter().any(|e| matches!(e, Report::GameStarted)));
        assert!(ev.iter().any(|e| matches!(e, Report::Log { line, .. } if line.contains("OutOfMemoryError"))));
        assert!(std::fs::read_to_string(dir.path().join("logs").join("x.log")).unwrap().contains("simulado"));
    }

    #[tokio::test]
    async fn launch_saida_zero_nao_e_crash() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        let cmd = vec!["cmd".to_string(), "/C".into(), "exit 0".into()];
        #[cfg(not(windows))]
        let cmd = vec!["sh".to_string(), "-c".into(), "exit 0".into()];
        let h = launch(cmd, dir.path().to_path_buf(), dir.path().join("x.log"), std::sync::Arc::new(|_| {})).await.unwrap();
        let info = h.task.await.unwrap();
        assert!(!info.crashed && info.summary.is_none());
    }

    #[tokio::test]
    async fn executavel_inexistente_da_erro_amigavel() {
        let dir = tempfile::tempdir().unwrap();
        let r = launch(vec!["/nao/existe/java.exe".into()], dir.path().to_path_buf(), dir.path().join("x.log"), std::sync::Arc::new(|_| {})).await;
        assert!(r.err().unwrap().0.contains("Java"));
    }
}
