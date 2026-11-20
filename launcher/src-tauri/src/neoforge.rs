//! Instalação do NeoForge via installer oficial em modo headless (`--installClient`).
//! O installer roda os "processors" (binpatch do client) — reimplementar isso seria frágil.
//! Depois lemos o version.json gerado e montamos o comando de launch nós mesmos.
use crate::download::{self, Hash, Task};
use crate::error::{AppError, Result};
use crate::java::hide_window;
use crate::mojang::{self, missing_libraries};
use crate::report::{stage, Report, Reporter};
use crate::settings::Dirs;
use crate::version::{Features, VersionJson};
use reqwest::Client;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

const MAVEN: &str = "https://maven.neoforged.net/releases/net/neoforged/neoforge";
const MARKER: &str = ".stl-installed";

pub fn profile_id(nf: &str) -> String {
    format!("neoforge-{nf}")
}

fn profile_json_path(dirs: &Dirs, nf: &str) -> PathBuf {
    let id = profile_id(nf);
    dirs.version_dir(&id).join(format!("{id}.json"))
}

fn marker_path(dirs: &Dirs, nf: &str) -> PathBuf {
    dirs.version_dir(&profile_id(nf)).join(MARKER)
}

/// Aceita só versões no formato esperado (evita injetar caminhos/URLs a partir do manifest).
pub fn valid_version(nf: &str) -> bool {
    !nf.is_empty()
        && nf.len() <= 40
        && nf.split('.').count() >= 3
        && nf.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

async fn load_profile(dirs: &Dirs, nf: &str) -> Option<VersionJson> {
    let b = tokio::fs::read(profile_json_path(dirs, nf)).await.ok()?;
    mojang::parse_version(&b).ok()
}

/// Roda o installer oficial. `java` deve ser o Java 21.
async fn run_installer(client: &Client, dirs: &Dirs, java: &std::path::Path, nf: &str, rep: &Reporter) -> Result<()> {
    let url = format!("{MAVEN}/{nf}/neoforge-{nf}-installer.jar");
    let jar = dirs.cache().join(format!("neoforge-{nf}-installer.jar"));
    // SHA1 publicado no Maven (se indisponível, segue sem — HTTPS já protege o transporte).
    let hash = match download::get_bytes(client, &format!("{url}.sha1"), Duration::from_secs(15)).await {
        Ok(b) => {
            let t = String::from_utf8_lossy(&b).trim().to_string();
            if t.len() >= 40 && t[..40].chars().all(|c| c.is_ascii_hexdigit()) { Hash::Sha1(t[..40].to_string()) } else { Hash::None }
        }
        Err(_) => Hash::None,
    };
    stage(rep, format!("Baixando o NeoForge {nf}..."));
    let _ = tokio::fs::remove_file(&jar).await;
    download::run_downloads(client, vec![Task { url, dest: jar.clone(), hash, size: 0 }], 1, "NeoForge", rep).await?;

    // O installer exige que o diretório pareça uma instalação do launcher oficial.
    let profiles = dirs.root.join("launcher_profiles.json");
    if !profiles.exists() {
        tokio::fs::write(&profiles, br#"{"profiles":{},"settings":{},"version":3}"#).await?;
    }

    stage(rep, "Instalando o NeoForge (pode levar alguns minutos)...");
    let mut cmd = Command::new(java);
    cmd.arg("-jar").arg(&jar).arg("--installClient").arg(&dirs.root);
    cmd.current_dir(dirs.cache()).stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null()).kill_on_drop(true);
    hide_window(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| AppError::new(format!("Não foi possível executar o Java para instalar o NeoForge: {e}")))?;

    let tail = std::sync::Arc::new(std::sync::Mutex::new(VecDeque::<String>::new()));
    let mut readers = Vec::new();
    macro_rules! pump {
        ($s:expr) => {{
            let (rep, tail) = (rep.clone(), tail.clone());
            let r = BufReader::new($s);
            readers.push(tokio::spawn(async move {
                let mut lines = r.lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    {
                        let mut t = tail.lock().unwrap();
                        if t.len() >= 15 {
                            t.pop_front();
                        }
                        t.push_back(l.clone());
                    }
                    rep(Report::Log { line: l, stream: "installer".into() });
                }
            }));
        }};
    }
    pump!(child.stdout.take().unwrap());
    pump!(child.stderr.take().unwrap());

    let status = match tokio::time::timeout(Duration::from_secs(20 * 60), child.wait()).await {
        Ok(r) => r?,
        Err(_) => return Err(AppError::new("A instalação do NeoForge demorou demais e foi cancelada. Tente \"Reparar instalação\".")),
    };
    for r in readers {
        let _ = r.await;
    }
    if !status.success() {
        let last = tail.lock().unwrap().iter().rev().find(|l| !l.trim().is_empty()).cloned().unwrap_or_default();
        return Err(AppError::new(format!(
            "A instalação do NeoForge falhou (código {}). Verifique sua internet e o antivírus e tente \"Reparar instalação\". Último aviso: {last}",
            status.code().map_or("?".into(), |c| c.to_string())
        )));
    }
    let _ = tokio::fs::remove_file(&jar).await;
    Ok(())
}

/// Garante o NeoForge instalado e devolve o perfil MESCLADO (vanilla + NeoForge).
/// `full` força reinstalação completa (usado em "Reparar instalação").
pub async fn ensure(
    client: &Client,
    dirs: &Dirs,
    java: &std::path::Path,
    parent: &VersionJson,
    nf: &str,
    features: &Features,
    full: bool,
    rep: &Reporter,
) -> Result<VersionJson> {
    if !valid_version(nf) {
        return Err(AppError::new(format!("Versão do NeoForge inválida no pack: \"{nf}\". Avise o administrador.")));
    }
    stage(rep, "Verificando o NeoForge...");

    let mut child = if full || !marker_path(dirs, nf).exists() { None } else { load_profile(dirs, nf).await };

    // Já instalado: confere as libraries; as que têm URL baixamos, as geradas pelo installer exigem reinstalar.
    if let Some(c) = &child {
        let merged = crate::version::merge(parent, c);
        let (tasks, generated) = {
            let (m, f, d) = (merged.clone(), features.clone(), dirs.clone());
            tokio::task::spawn_blocking(move || missing_libraries(&m, &f, &d, false)).await?
        };
        if generated > 0 {
            child = None;
        } else if !tasks.is_empty() {
            stage(rep, "Baixando bibliotecas do NeoForge...");
            download::run_downloads(client, tasks, 8, "Bibliotecas", rep).await?;
        }
    }

    let child = match child {
        Some(c) => c,
        None => {
            let _ = tokio::fs::remove_file(marker_path(dirs, nf)).await;
            run_installer(client, dirs, java, nf, rep).await?;
            let c = load_profile(dirs, nf)
                .await
                .ok_or_else(|| AppError::new("O instalador terminou mas o perfil do NeoForge não foi criado. Tente \"Reparar instalação\"."))?;
            let merged = crate::version::merge(parent, &c);
            let (m, f, d) = (merged.clone(), features.clone(), dirs.clone());
            let (tasks, generated) = tokio::task::spawn_blocking(move || missing_libraries(&m, &f, &d, false)).await?;
            if !tasks.is_empty() {
                download::run_downloads(client, tasks, 8, "Bibliotecas", rep).await?;
            }
            if generated > 0 {
                return Err(AppError::new("A instalação do NeoForge ficou incompleta (arquivos gerados ausentes). O antivírus pode ter removido arquivos; adicione a pasta do launcher às exceções e tente de novo."));
            }
            tokio::fs::write(marker_path(dirs, nf), b"ok").await?;
            c
        }
    };
    Ok(crate::version::merge(parent, &child))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn versoes_validas() {
        assert!(valid_version("21.1.172"));
        assert!(valid_version("21.1.172-beta"));
        for bad in ["", "21.1", "../x.y.z", "21.1.1/../..", "a b c", "21.1.172\n"] {
            assert!(!valid_version(bad), "{bad:?}");
        }
    }
}
