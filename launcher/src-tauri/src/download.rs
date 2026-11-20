//! Motor de download: paralelo, com retomada (.part + Range), verificação de hash e progresso/ETA.
use crate::error::{is_fatal_io, io_message, reqwest_message, AppError, Result};
use crate::report::{Report, Reporter};
use futures_util::StreamExt;
use reqwest::{header, Client, StatusCode};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;

const MAX_ATTEMPTS: u32 = 4;
const CHUNK_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq)]
pub enum Hash {
    None,
    Sha1(String),
    Sha256(String),
}

#[derive(Clone, Debug)]
pub struct Task {
    pub url: String,
    pub dest: PathBuf,
    pub hash: Hash,
    /// Tamanho esperado em bytes (0 = desconhecido).
    pub size: u64,
}

pub fn build_client() -> Result<Client> {
    Client::builder()
        .user_agent(concat!("STLauncher/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| AppError::new(format!("Não foi possível iniciar o cliente de rede: {e}")))
}

// ---------- hash ----------

fn hash_reader<D: Digest, R: Read>(mut r: R) -> std::io::Result<String> {
    let mut h = D::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

pub fn sha1_file(p: &Path) -> std::io::Result<String> {
    hash_reader::<Sha1, _>(std::fs::File::open(p)?)
}
pub fn sha256_file(p: &Path) -> std::io::Result<String> {
    hash_reader::<Sha256, _>(std::fs::File::open(p)?)
}

/// Confere o hash de um arquivo (bloqueante). Hash::None => sempre true.
pub fn hash_matches(p: &Path, h: &Hash) -> std::io::Result<bool> {
    Ok(match h {
        Hash::None => true,
        Hash::Sha1(x) => sha1_file(p)?.eq_ignore_ascii_case(x),
        Hash::Sha256(x) => sha256_file(p)?.eq_ignore_ascii_case(x),
    })
}

/// Verificação rápida (só existência e tamanho) ou completa (com hash). Bloqueante.
pub fn file_ok(p: &Path, size: u64, h: &Hash, full: bool) -> bool {
    match std::fs::metadata(p) {
        Ok(m) if m.is_file() => {
            if size > 0 && m.len() != size {
                return false;
            }
            if full || size == 0 {
                hash_matches(p, h).unwrap_or(false)
            } else {
                true
            }
        }
        _ => false,
    }
}

pub fn part_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

// ---------- progresso ----------

pub struct Tracker {
    done: AtomicU64,
    files_done: AtomicU64,
    total: u64,
    files_total: u64,
}
impl Tracker {
    fn add(&self, n: u64) {
        self.done.fetch_add(n, Ordering::Relaxed);
    }
    fn sub(&self, n: u64) {
        // Nunca fica negativo.
        let _ = self.done.try_update(Ordering::Relaxed, Ordering::Relaxed, |v| Some(v.saturating_sub(n)));
    }
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn snapshot(label: &str, tr: &Tracker, speed: u64) -> Report {
    let done = tr.done.load(Ordering::Relaxed);
    let eta = if speed > 0 && tr.total > done { Some((tr.total - done) / speed) } else { None };
    Report::Progress {
        label: label.to_string(),
        done_bytes: done,
        total_bytes: tr.total,
        files_done: tr.files_done.load(Ordering::Relaxed),
        files_total: tr.files_total,
        speed_bps: speed,
        eta_secs: eta,
    }
}

// ---------- download de um arquivo ----------

enum Fail {
    Retry(AppError),
    Fatal(AppError),
}

fn retry_net(e: reqwest::Error) -> Fail {
    Fail::Retry(AppError::new(reqwest_message(&e)))
}
fn io_fail(e: std::io::Error) -> Fail {
    let err = AppError::new(io_message(&e));
    if is_fatal_io(&e) { Fail::Fatal(err) } else { Fail::Retry(err) }
}

async fn attempt(client: &Client, t: &Task, tr: &Tracker, counted: &mut u64) -> std::result::Result<(), Fail> {
    let part = part_path(&t.dest);
    let mut existing = tokio::fs::metadata(&part).await.map(|m| m.len()).unwrap_or(0);
    if t.size > 0 && existing > t.size {
        let _ = tokio::fs::remove_file(&part).await;
        existing = 0;
    }

    if !(t.size > 0 && existing == t.size) {
        let mut req = client.get(&t.url);
        if existing > 0 {
            req = req.header(header::RANGE, format!("bytes={existing}-"));
        }
        let resp = req.send().await.map_err(retry_net)?;
        let status = resp.status();
        let append = match status {
            StatusCode::PARTIAL_CONTENT if existing > 0 => true,
            StatusCode::OK => false, // servidor ignorou o Range (ou não havia .part): recomeça do zero
            StatusCode::RANGE_NOT_SATISFIABLE => {
                let _ = tokio::fs::remove_file(&part).await;
                return Err(Fail::Retry(AppError::new("Retomada de download recusada; recomeçando.")));
            }
            s if s == StatusCode::NOT_FOUND || s == StatusCode::GONE => {
                return Err(Fail::Fatal(AppError::new(format!("Arquivo não encontrado no servidor (HTTP {}): {}", s.as_u16(), t.url))));
            }
            s if s.is_client_error() && s != StatusCode::REQUEST_TIMEOUT && s != StatusCode::TOO_MANY_REQUESTS => {
                return Err(Fail::Fatal(AppError::new(format!("O servidor recusou o download (HTTP {}): {}", s.as_u16(), t.url))));
            }
            s => {
                return Err(Fail::Retry(AppError::new(format!("O servidor respondeu HTTP {} ao baixar {}", s.as_u16(), t.url))));
            }
        };
        let mut file = if append {
            tokio::fs::OpenOptions::new().append(true).open(&part).await.map_err(io_fail)?
        } else {
            tokio::fs::File::create(&part).await.map_err(io_fail)?
        };
        if append {
            *counted = existing;
            tr.add(existing);
        }
        let mut stream = resp.bytes_stream();
        loop {
            match tokio::time::timeout(CHUNK_TIMEOUT, stream.next()).await {
                Err(_) => return Err(Fail::Retry(AppError::new("A conexão travou (sem dados por 30s)."))),
                Ok(None) => break,
                Ok(Some(Err(e))) => return Err(retry_net(e)),
                Ok(Some(Ok(chunk))) => {
                    file.write_all(&chunk).await.map_err(io_fail)?;
                    *counted += chunk.len() as u64;
                    tr.add(chunk.len() as u64);
                }
            }
        }
        file.flush().await.map_err(io_fail)?;
        drop(file);
    } else {
        // .part já completo (download anterior terminou mas não foi renomeado)
        *counted = existing;
        tr.add(existing);
    }

    // Verificação de tamanho e hash.
    let len = tokio::fs::metadata(&part).await.map_err(io_fail)?.len();
    if t.size > 0 && len != t.size {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(Fail::Retry(AppError::new(format!("Download incompleto ({len} de {} bytes).", t.size))));
    }
    if t.hash != Hash::None {
        let (p, h) = (part.clone(), t.hash.clone());
        let ok = tokio::task::spawn_blocking(move || hash_matches(&p, &h))
            .await
            .map_err(|e| Fail::Retry(e.into()))?
            .map_err(io_fail)?;
        if !ok {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(Fail::Retry(AppError::new(format!(
                "Arquivo corrompido (hash não confere): {}",
                t.dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
            ))));
        }
    }
    tokio::fs::rename(&part, &t.dest).await.map_err(io_fail)?;
    Ok(())
}

/// Baixa um arquivo com até 4 tentativas (backoff 2s/4s/8s) e retomada do .part.
pub async fn download_one(client: &Client, t: &Task, tr: &Tracker) -> Result<()> {
    if let Some(p) = t.dest.parent() {
        tokio::fs::create_dir_all(p).await?;
    }
    let mut last = AppError::new("falha desconhecida");
    for n in 0..MAX_ATTEMPTS {
        if n > 0 {
            tokio::time::sleep(Duration::from_secs(1 << n)).await;
        }
        let mut counted = 0u64;
        match attempt(client, t, tr, &mut counted).await {
            Ok(()) => {
                tr.files_done.fetch_add(1, Ordering::Relaxed);
                return Ok(());
            }
            Err(Fail::Fatal(e)) => {
                tr.sub(counted);
                return Err(e);
            }
            Err(Fail::Retry(e)) => {
                // Esta tentativa será recontada a partir do .part na próxima.
                tr.sub(counted);
                last = e;
            }
        }
    }
    Err(last.context("Falha após várias tentativas"))
}

/// Executa várias tarefas em paralelo emitindo progresso. Para na primeira falha.
pub async fn run_downloads(client: &Client, tasks: Vec<Task>, parallel: usize, label: &str, rep: &Reporter) -> Result<()> {
    if tasks.is_empty() {
        return Ok(());
    }
    let tr = Arc::new(Tracker {
        done: AtomicU64::new(0),
        files_done: AtomicU64::new(0),
        total: tasks.iter().map(|t| t.size).sum(),
        files_total: tasks.len() as u64,
    });

    // Emite progresso a cada 250ms com velocidade média dos últimos ~3s.
    let ticker = {
        let (tr, rep, label) = (tr.clone(), rep.clone(), label.to_string());
        tokio::spawn(async move {
            let mut samples: VecDeque<(Instant, u64)> = VecDeque::new();
            let mut iv = tokio::time::interval(Duration::from_millis(250));
            loop {
                iv.tick().await;
                let now = Instant::now();
                let done = tr.done.load(Ordering::Relaxed);
                samples.push_back((now, done));
                while samples.len() > 2 && now.duration_since(samples[0].0) > Duration::from_secs(3) {
                    samples.pop_front();
                }
                let (t0, d0) = samples[0];
                let dt = now.duration_since(t0).as_secs_f64();
                let speed = if dt > 0.2 && done >= d0 { ((done - d0) as f64 / dt) as u64 } else { 0 };
                rep(snapshot(&label, &tr, speed));
            }
        })
    };
    let _guard = AbortOnDrop(ticker);

    let mut results = futures_util::stream::iter(tasks)
        .map(|t| {
            let (c, tr) = (client.clone(), tr.clone());
            async move { download_one(&c, &t, &tr).await }
        })
        .buffer_unordered(parallel.max(1));
    while let Some(r) = results.next().await {
        r?; // ao retornar, o stream é descartado e cancela os downloads em andamento
    }
    rep(snapshot(label, &tr, 0));
    Ok(())
}

/// GET simples de JSON com timeout (APIs pequenas).
pub async fn get_json<T: serde::de::DeserializeOwned>(client: &Client, url: &str, timeout: Duration) -> Result<T> {
    let resp = client.get(url).timeout(timeout).send().await?.error_for_status()?;
    let bytes = resp.bytes().await?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Baixa um arquivo pequeno para a memória.
pub async fn get_bytes(client: &Client, url: &str, timeout: Duration) -> Result<Vec<u8>> {
    let resp = client.get(url).timeout(timeout).send().await?.error_for_status()?;
    Ok(resp.bytes().await?.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    fn quiet() -> Reporter {
        Arc::new(|_| {})
    }

    /// Servidor HTTP mínimo: serve `body` com suporte a Range; `corrupt_first` corrompe a 1ª resposta;
    /// `cut_first` interrompe a 1ª resposta na metade (simula queda de conexão).
    async fn serve(body: Vec<u8>, cut_first: bool, honor_range: bool) -> (String, Arc<AtomicU64>) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let hits = Arc::new(AtomicU64::new(0));
        let h2 = hits.clone();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = l.accept().await.unwrap();
                let body = body.clone();
                let n = h2.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let k = s.read(&mut buf).await.unwrap();
                    let req = String::from_utf8_lossy(&buf[..k]).to_string();
                    let start = req
                        .lines()
                        .find_map(|l| l.to_lowercase().strip_prefix("range: bytes=").map(|r| r.trim_end_matches('-').parse::<usize>().unwrap()))
                        .filter(|_| honor_range);
                    let (status, data, extra) = match start {
                        Some(st) => ("206 Partial Content", body[st..].to_vec(), format!("Content-Range: bytes {st}-{}/{}\r\n", body.len() - 1, body.len())),
                        None => ("200 OK", body.clone(), String::new()),
                    };
                    let head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n", data.len());
                    s.write_all(head.as_bytes()).await.unwrap();
                    if cut_first && n == 0 {
                        s.write_all(&data[..data.len() / 2]).await.unwrap();
                        let _ = s.flush().await;
                        // fecha sem enviar o resto
                    } else {
                        s.write_all(&data).await.unwrap();
                    }
                });
            }
        });
        (format!("http://{addr}/f.bin"), hits)
    }

    fn sample(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 31 % 251) as u8).collect()
    }

    #[tokio::test]
    async fn baixa_e_valida_sha256() {
        let body = sample(300_000);
        let want = hex::encode(Sha256::digest(&body));
        let (url, _) = serve(body.clone(), false, true).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("sub").join("a.bin");
        let task = Task { url, dest: dest.clone(), hash: Hash::Sha256(want), size: body.len() as u64 };
        run_downloads(&build_client().unwrap(), vec![task], 4, "t", &quiet()).await.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert!(!part_path(&dest).exists());
    }

    #[tokio::test]
    async fn hash_errado_falha_e_nao_deixa_arquivo() {
        let body = sample(1000);
        let (url, hits) = serve(body.clone(), false, true).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("a.bin");
        let task = Task { url, dest: dest.clone(), hash: Hash::Sha1("0".repeat(40)), size: 1000 };
        // usa download_one direto para não esperar os 2+4+8s de backoff completos: aceitável (14s) mas limitamos
        let tr = Tracker { done: AtomicU64::new(0), files_done: AtomicU64::new(0), total: 1000, files_total: 1 };
        let r = tokio::time::timeout(Duration::from_secs(30), download_one(&build_client().unwrap(), &task, &tr)).await.unwrap();
        let err = r.unwrap_err().to_string();
        assert!(err.contains("corrompido"), "{err}");
        assert!(!dest.exists() && !part_path(&dest).exists());
        assert_eq!(hits.load(Ordering::SeqCst), 4); // 4 tentativas
    }

    #[tokio::test]
    async fn retoma_de_part_existente_com_range() {
        let body = sample(100_000);
        let want = hex::encode(Sha1::digest(&body));
        let (url, _) = serve(body.clone(), false, true).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("a.bin");
        std::fs::write(part_path(&dest), &body[..40_000]).unwrap();
        let task = Task { url, dest: dest.clone(), hash: Hash::Sha1(want), size: 100_000 };
        run_downloads(&build_client().unwrap(), vec![task], 1, "t", &quiet()).await.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body);
    }

    #[tokio::test]
    async fn servidor_sem_range_recomeca_do_zero() {
        let body = sample(50_000);
        let want = hex::encode(Sha256::digest(&body));
        let (url, _) = serve(body.clone(), false, false).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("a.bin");
        std::fs::write(part_path(&dest), b"lixo-de-download-anterior").unwrap();
        let task = Task { url, dest: dest.clone(), hash: Hash::Sha256(want), size: 50_000 };
        run_downloads(&build_client().unwrap(), vec![task], 1, "t", &quiet()).await.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body);
    }

    #[tokio::test]
    async fn queda_de_conexao_retoma_na_tentativa_seguinte() {
        let body = sample(200_000);
        let want = hex::encode(Sha256::digest(&body));
        let (url, hits) = serve(body.clone(), true, true).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("a.bin");
        let task = Task { url, dest: dest.clone(), hash: Hash::Sha256(want), size: 200_000 };
        run_downloads(&build_client().unwrap(), vec![task], 1, "t", &quiet()).await.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert_eq!(hits.load(Ordering::SeqCst), 2); // 1ª cortada, 2ª completou
    }

    #[tokio::test]
    async fn http_404_e_fatal_sem_retentar() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let hits = Arc::new(AtomicU64::new(0));
        let h2 = hits.clone();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = l.accept().await.unwrap();
                h2.fetch_add(1, Ordering::SeqCst);
                let mut b = [0u8; 2048];
                let _ = s.read(&mut b).await;
                let _ = s.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let task = Task { url: format!("http://{addr}/x"), dest: dir.path().join("x"), hash: Hash::None, size: 0 };
        let err = run_downloads(&build_client().unwrap(), vec![task], 1, "t", &quiet()).await.unwrap_err();
        assert!(err.to_string().contains("não encontrado"), "{err}");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn file_ok_rapido_e_completo() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        std::fs::write(&p, b"abc").unwrap();
        let good = Hash::Sha1(hex::encode(Sha1::digest(b"abc")));
        let bad = Hash::Sha1("0".repeat(40));
        assert!(file_ok(&p, 3, &good, true));
        assert!(!file_ok(&p, 3, &bad, true));
        assert!(file_ok(&p, 3, &bad, false)); // rápido ignora hash
        assert!(!file_ok(&p, 4, &good, false)); // tamanho diferente
        assert!(!file_ok(&dir.path().join("nada"), 3, &good, false));
    }
}
