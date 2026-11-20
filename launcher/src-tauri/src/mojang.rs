//! Instalação do Minecraft vanilla a partir do manifest oficial da Mojang.
use crate::download::{self, file_ok, Hash, Task};
use crate::error::{AppError, Result};
use crate::report::{stage, Reporter};
use crate::settings::Dirs;
use crate::version::{active_libraries, Artifact, Features, VersionJson};
use reqwest::Client;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

const MANIFEST_URL: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
const ASSETS_URL: &str = "https://resources.download.minecraft.net";
const PARALLEL: usize = 16;

#[derive(Deserialize)]
struct Manifest {
    versions: Vec<Entry>,
}
#[derive(Deserialize)]
struct Entry {
    id: String,
    url: String,
    sha1: String,
}
#[derive(Deserialize)]
struct AssetIndex {
    objects: HashMap<String, AssetObject>,
}
#[derive(Deserialize)]
struct AssetObject {
    hash: String,
    size: u64,
}

pub fn client_jar(dirs: &Dirs, id: &str) -> PathBuf {
    dirs.version_dir(id).join(format!("{id}.jar"))
}

pub fn parse_version(bytes: &[u8]) -> Result<VersionJson> {
    Ok(serde_json::from_slice(bytes)?)
}

/// Carrega o version.json local; se não existir (ou `force`), baixa da Mojang e verifica o SHA1.
pub async fn ensure_version_json(client: &Client, dirs: &Dirs, mc: &str, force: bool) -> Result<VersionJson> {
    let path = dirs.version_dir(mc).join(format!("{mc}.json"));
    if !force {
        if let Ok(b) = tokio::fs::read(&path).await {
            if let Ok(v) = parse_version(&b) {
                return Ok(v);
            }
        }
    }
    let m: Manifest = download::get_json(client, MANIFEST_URL, Duration::from_secs(20)).await?;
    let e = m
        .versions
        .into_iter()
        .find(|e| e.id == mc)
        .ok_or_else(|| AppError::new(format!("A versão {mc} do Minecraft não existe no manifest da Mojang.")))?;
    let bytes = download::get_bytes(client, &e.url, Duration::from_secs(30)).await?;
    let got = hex::encode(<sha1::Sha1 as sha1::Digest>::digest(&bytes));
    if !got.eq_ignore_ascii_case(&e.sha1) {
        return Err(AppError::new("O arquivo de versão do Minecraft veio corrompido. Tente de novo."));
    }
    let v = parse_version(&bytes)?;
    tokio::fs::create_dir_all(path.parent().unwrap()).await?;
    write_atomic(&path, &bytes).await?;
    Ok(v)
}

pub async fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = download::part_path(path);
    tokio::fs::write(&tmp, bytes).await?;
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

fn sha1_hash(s: &Option<String>) -> Hash {
    s.as_ref().map(|x| Hash::Sha1(x.clone())).unwrap_or(Hash::None)
}

/// Tarefa de download para um artefato de biblioteca, se o arquivo local estiver ausente/inválido.
pub fn lib_task(libraries: &Path, a: &Artifact) -> Option<Task> {
    if a.url.is_empty() {
        return None; // gerado localmente pelo installer do NeoForge
    }
    Some(Task {
        url: a.url.clone(),
        dest: libraries.join(a.path.as_deref().unwrap_or_default()),
        hash: sha1_hash(&a.sha1),
        size: a.size,
    })
}

/// Libs (do perfil já mesclado ou vanilla) que faltam em disco: (com url para baixar, sem url = geradas pelo installer).
pub fn missing_libraries(v: &VersionJson, features: &Features, dirs: &Dirs, full: bool) -> (Vec<Task>, usize) {
    let mut tasks = Vec::new();
    let mut generated = 0;
    let mut seen = HashSet::new();
    for (_, a) in active_libraries(v, features) {
        let dest = dirs.libraries().join(a.path.as_deref().unwrap_or_default());
        if !seen.insert(dest.clone()) {
            continue;
        }
        let hash = sha1_hash(&a.sha1);
        if file_ok(&dest, a.size, &hash, full) {
            continue;
        }
        match lib_task(&dirs.libraries(), &a) {
            Some(t) => tasks.push(t),
            None => generated += 1,
        }
    }
    (tasks, generated)
}

/// Garante client.jar + libraries + assets do Minecraft. `full` revalida hashes de tudo.
pub async fn ensure_game(client: &Client, dirs: &Dirs, v: &VersionJson, features: &Features, full: bool, rep: &Reporter) -> Result<()> {
    stage(rep, "Verificando arquivos do Minecraft...");
    let mut tasks: Vec<Task> = Vec::new();

    // client.jar
    if let Some(c) = v.downloads.as_ref().and_then(|d| d.client.as_ref()) {
        let dest = client_jar(dirs, &v.id);
        let hash = sha1_hash(&c.sha1);
        let (d2, h2, sz) = (dest.clone(), hash.clone(), c.size);
        if !tokio::task::spawn_blocking(move || file_ok(&d2, sz, &h2, full)).await? {
            tasks.push(Task { url: c.url.clone(), dest, hash, size: c.size });
        }
    } else {
        return Err(AppError::new("Versão do Minecraft sem client.jar (arquivo de versão inválido)."));
    }

    // libraries
    {
        let (v2, f2, d2) = (v.clone(), features.clone(), dirs.clone());
        let (libs, _) = tokio::task::spawn_blocking(move || missing_libraries(&v2, &f2, &d2, full)).await?;
        tasks.extend(libs);
    }

    // asset index + objetos
    let idx = v.asset_index.as_ref().ok_or_else(|| AppError::new("Versão do Minecraft sem índice de assets."))?;
    let idx_path = dirs.assets().join("indexes").join(format!("{}.json", idx.id));
    let idx_hash = Hash::Sha1(idx.sha1.clone());
    if !file_ok(&idx_path, idx.size, &idx_hash, true) {
        download::run_downloads(client, vec![Task { url: idx.url.clone(), dest: idx_path.clone(), hash: idx_hash, size: idx.size }], 1, "Índice de assets", rep).await?;
    }
    let index: AssetIndex = serde_json::from_slice(&tokio::fs::read(&idx_path).await?)?;
    {
        let objects_dir = dirs.assets().join("objects");
        let objs: HashMap<String, u64> = index.objects.into_values().map(|o| (o.hash, o.size)).collect(); // dedupe por hash
        let missing = tokio::task::spawn_blocking(move || {
            objs.into_iter()
                .filter_map(|(hash, size)| {
                    let dest = objects_dir.join(&hash[..2]).join(&hash);
                    let h = Hash::Sha1(hash.clone());
                    if file_ok(&dest, size, &h, full) {
                        None
                    } else {
                        Some(Task { url: format!("{ASSETS_URL}/{}/{}", &hash[..2], hash), dest, hash: h, size })
                    }
                })
                .collect::<Vec<_>>()
        })
        .await?;
        tasks.extend(missing);
    }

    if tasks.is_empty() {
        return Ok(());
    }
    stage(rep, format!("Baixando Minecraft {}...", v.id));
    // Maiores primeiro: melhor uso da banda e o ETA estabiliza mais rápido.
    tasks.sort_by(|a, b| b.size.cmp(&a.size));
    download::run_downloads(client, tasks, PARALLEL, "Minecraft", rep).await
}
