//! Modpack: manifest da API, diff (sha256) e sincronização com a pasta do jogo.
use crate::download::{self, sha256_file, Hash, Task};
use crate::error::{AppError, Result};
use crate::report::{stage, Reporter};
use crate::settings::Dirs;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Pastas de topo permitidas (mesma whitelist da API).
pub const ALLOWED_ROOTS: [&str; 4] = ["mods", "config", "resourcepacks", "shaderpacks"];
/// Arquivos de raiz permitidos. Só são (re)escritos quando o pack os altera.
pub const ALLOWED_FILES: [&str; 1] = ["servers.dat"];
/// Pasta 100% gerenciada: arquivos que não estão no manifest são removidos.
const MANAGED_DIR: &str = "mods";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub pack_version: String,
    pub mc_version: String,
    pub neoforge_version: String,
    pub server_address: String,
    #[serde(default = "default_min_ram")]
    pub min_ram_mb: u32,
    #[serde(default)]
    pub news: String,
    pub files: Vec<PackFile>,
}
fn default_min_ram() -> u32 {
    4096
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PackFile {
    pub path: String,
    pub sha256: String,
    pub size: u64,
    pub url: String,
}

// ---------- validação (o manifest vem da rede: nunca confie nele) ----------

/// Mesmas regras da API (api/src/lib/paths.js). Impede escrever fora da pasta do jogo.
pub fn validate_path(p: &str) -> std::result::Result<(), String> {
    if p.is_empty() || p.len() > 240 {
        return Err(format!("caminho inválido: {p:?}"));
    }
    if p.starts_with('/') {
        return Err(format!("caminho absoluto não permitido: {p}"));
    }
    let parts: Vec<&str> = p.split('/').collect();
    for part in &parts {
        if part.is_empty() || *part == "." || *part == ".." {
            return Err(format!("caminho inválido: {p}"));
        }
        if part.chars().any(|c| "<>:\"|?*\\".contains(c) || (c as u32) < 0x20) {
            return Err(format!("caractere inválido no caminho: {p}"));
        }
        if part.ends_with('.') || part.ends_with(' ') {
            return Err(format!("nome termina com ponto/espaço: {p}"));
        }
        let stem = part.split('.').next().unwrap_or("").to_ascii_lowercase();
        let reserved = ["con", "prn", "aux", "nul"].contains(&stem.as_str())
            || ((stem.starts_with("com") || stem.starts_with("lpt")) && stem.len() == 4 && stem.as_bytes()[3].is_ascii_digit() && stem.as_bytes()[3] != b'0');
        if reserved {
            return Err(format!("nome reservado do Windows: {p}"));
        }
    }
    if parts.len() == 1 {
        if !ALLOWED_FILES.contains(&p) {
            return Err(format!("arquivo de raiz não permitido: {p}"));
        }
    } else if !ALLOWED_ROOTS.contains(&parts[0]) {
        return Err(format!("pasta não permitida no pack: {}/", parts[0]));
    }
    Ok(())
}

pub fn validate_manifest(m: &Manifest) -> Result<()> {
    let bad = |msg: String| AppError::new(format!("O modpack publicado é inválido ({msg}). Avise o administrador."));
    if m.mc_version.is_empty() || !m.mc_version.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return Err(bad(format!("versão do Minecraft: {:?}", m.mc_version)));
    }
    if !crate::neoforge::valid_version(&m.neoforge_version) {
        return Err(bad(format!("versão do NeoForge: {:?}", m.neoforge_version)));
    }
    let mut seen = HashSet::new();
    for f in &m.files {
        validate_path(&f.path).map_err(|e| bad(e))?;
        if f.sha256.len() != 64 || !f.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(bad(format!("sha256 inválido em {}", f.path)));
        }
        if !(f.url.starts_with('/') || f.url.starts_with("https://") || f.url.starts_with("http://")) || f.url.starts_with("//") {
            return Err(bad(format!("url inválida em {}", f.path)));
        }
        if !seen.insert(f.path.to_lowercase()) {
            return Err(bad(format!("caminho duplicado: {}", f.path)));
        }
    }
    Ok(())
}

/// URL absoluta de um arquivo: relativa ("/files/...") é resolvida contra a URL da API.
pub fn resolve_url(api: &str, url: &str) -> String {
    if url.starts_with("http://") || url.starts_with("https://") {
        url.to_string()
    } else {
        format!("{}{}", api.trim_end_matches('/'), url)
    }
}

pub async fn fetch_manifest(client: &Client, api: &str) -> Result<Manifest> {
    let url = format!("{}/api/pack/manifest", api.trim_end_matches('/'));
    let resp = client.get(&url).timeout(Duration::from_secs(15)).send().await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(AppError::new("O servidor de atualizações ainda não tem um modpack publicado."));
    }
    let resp = resp.error_for_status()?;
    let m: Manifest = serde_json::from_slice(&resp.bytes().await?)?;
    validate_manifest(&m)?;
    Ok(m)
}

// ---------- estado local ----------

pub async fn save_manifest_cache(dirs: &Dirs, m: &Manifest) -> Result<()> {
    tokio::fs::create_dir_all(dirs.state()).await?;
    crate::mojang::write_atomic(&dirs.state().join("last-manifest.json"), &serde_json::to_vec(m)?).await
}

pub async fn load_manifest_cache(dirs: &Dirs) -> Option<Manifest> {
    let b = tokio::fs::read(dirs.state().join("last-manifest.json")).await.ok()?;
    let m: Manifest = serde_json::from_slice(&b).ok()?;
    validate_manifest(&m).ok()?;
    Some(m)
}

/// path -> sha256 dos arquivos instalados na última sincronização completa.
pub async fn load_installed(dirs: &Dirs) -> HashMap<String, String> {
    match tokio::fs::read(dirs.state().join("installed-files.json")).await {
        Ok(b) => serde_json::from_slice(&b).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

async fn save_installed(dirs: &Dirs, files: &[PackFile]) -> Result<()> {
    let map: HashMap<&str, &str> = files.iter().map(|f| (f.path.as_str(), f.sha256.as_str())).collect();
    crate::mojang::write_atomic(&dirs.state().join("installed-files.json"), &serde_json::to_vec(&map)?).await
}

// ---------- diff ----------

#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    pub download: Vec<PackFile>,
    pub delete: Vec<String>,
}

/// Decide o que baixar e o que apagar. Função pura (testável).
///
/// * `local`: para cada arquivo do manifest que existe em disco, seu sha256
///   (string vazia se o tamanho já difere do esperado, o que dispensa calcular hash).
/// * `disk_managed`: TODOS os arquivos hoje em `mods/` (caminhos relativos, "/").
/// * `prev`: path -> sha256 da última sincronização.
pub fn plan_sync(files: &[PackFile], local: &HashMap<String, String>, disk_managed: &[String], prev: &HashMap<String, String>) -> Plan {
    let mut plan = Plan::default();
    let wanted: HashSet<String> = files.iter().map(|f| f.path.to_lowercase()).collect();

    for f in files {
        let needs = if ALLOWED_FILES.contains(&f.path.as_str()) {
            // servers.dat: o jogador pode ter adicionado servidores. Só sobrescreve se não existe
            // ou se o PACK mudou o arquivo desde a última sincronização.
            !local.contains_key(&f.path) || prev.get(&f.path).map(String::as_str) != Some(f.sha256.as_str())
        } else {
            local.get(&f.path).map(|h| !h.eq_ignore_ascii_case(&f.sha256)).unwrap_or(true)
        };
        if needs {
            plan.download.push(f.clone());
        }
    }

    // mods/: remove tudo que não está no manifest. Preserva "<arquivo>.part" de downloads a retomar.
    for p in disk_managed {
        let lower = p.to_lowercase();
        if wanted.contains(&lower) {
            continue;
        }
        if let Some(base) = lower.strip_suffix(".part") {
            if wanted.contains(base) {
                continue;
            }
        }
        plan.delete.push(p.clone());
    }
    // Demais pastas: remove só o que um pack anterior instalou e o atual não tem mais.
    for p in prev.keys() {
        let in_managed = p.split('/').next() == Some(MANAGED_DIR);
        if !in_managed && !ALLOWED_FILES.contains(&p.as_str()) && !wanted.contains(&p.to_lowercase()) {
            plan.delete.push(p.clone());
        }
    }
    plan.delete.sort();
    plan.delete.dedup();
    plan
}

// ---------- execução ----------

fn list_files(dir: &Path, rel: &str, out: &mut Vec<String>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let r = format!("{rel}/{name}");
            match e.file_type() {
                Ok(t) if t.is_dir() => list_files(&e.path(), &r, out),
                Ok(_) => out.push(r),
                Err(_) => {}
            }
        }
    }
}

/// Calcula (em paralelo) o sha256 dos arquivos do manifest que existem localmente.
fn hash_local(game: &Path, files: &[PackFile]) -> HashMap<String, String> {
    let next = AtomicUsize::new(0);
    let out = std::sync::Mutex::new(HashMap::new());
    let workers = std::thread::available_parallelism().map_or(2, |n| n.get()).clamp(1, 4);
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(f) = files.get(i) else { break };
                let p = game.join(&f.path);
                if let Ok(m) = std::fs::metadata(&p) {
                    if m.is_file() {
                        let h = if m.len() == f.size { sha256_file(&p).unwrap_or_default() } else { String::new() };
                        out.lock().unwrap().insert(f.path.clone(), h);
                    }
                }
            });
        }
    });
    out.into_inner().unwrap()
}

/// Sincroniza a pasta do jogo com o manifest: apaga o que sobrou e baixa o que falta/mudou.
pub async fn sync(client: &Client, dirs: &Dirs, api: &str, m: &Manifest, rep: &Reporter) -> Result<()> {
    stage(rep, "Verificando o modpack...");
    let game = dirs.game();
    tokio::fs::create_dir_all(&game).await?;
    let prev = load_installed(dirs).await;

    let (g2, files) = (game.clone(), m.files.clone());
    let plan = tokio::task::spawn_blocking(move || {
        let local = hash_local(&g2, &files);
        let mut managed = Vec::new();
        list_files(&g2.join(MANAGED_DIR), MANAGED_DIR, &mut managed);
        plan_sync(&files, &local, &managed, &prev)
    })
    .await?;

    for p in &plan.delete {
        match tokio::fs::remove_file(game.join(p)).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(AppError::from(e).context(format!("Não consegui remover {p}"))),
        }
    }

    if !plan.download.is_empty() {
        stage(rep, format!("Baixando o modpack ({} arquivos)...", plan.download.len()));
        let mut tasks: Vec<Task> = plan
            .download
            .iter()
            .map(|f| Task { url: resolve_url(api, &f.url), dest: game.join(&f.path), hash: Hash::Sha256(f.sha256.clone()), size: f.size })
            .collect();
        tasks.sort_by(|a, b| b.size.cmp(&a.size));
        download::run_downloads(client, tasks, 8, "Modpack", rep).await?;
    }
    save_installed(dirs, &m.files).await
}

/// Modo offline: confere se o pack instalado está completo (existência + tamanho).
pub fn verify_installed_quick(dirs: &Dirs, m: &Manifest) -> Result<()> {
    let game = dirs.game();
    let missing = m
        .files
        .iter()
        .filter(|f| !ALLOWED_FILES.contains(&f.path.as_str()))
        .filter(|f| std::fs::metadata(game.join(&f.path)).map(|md| md.len() != f.size).unwrap_or(true))
        .count();
    if missing > 0 {
        return Err(AppError::new(format!("Sem conexão e o modpack instalado está incompleto ({missing} arquivos faltando). Conecte-se à internet para atualizar.")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pf(path: &str, sha: &str) -> PackFile {
        PackFile { path: path.into(), sha256: sha.repeat(64).chars().take(64).collect(), size: 10, url: format!("/files/{path}") }
    }
    fn map(v: &[(&str, &PackFile)]) -> HashMap<String, String> {
        v.iter().map(|(p, f)| (p.to_string(), f.sha256.clone())).collect()
    }

    #[test]
    fn baixa_novos_e_alterados_pula_iguais() {
        let (a, b, c) = (pf("mods/a.jar", "a"), pf("mods/b.jar", "b"), pf("config/c.toml", "c"));
        let mut local = map(&[("mods/a.jar", &a)]);
        local.insert("mods/b.jar".into(), "0".repeat(64)); // hash diferente
        let plan = plan_sync(&[a.clone(), b.clone(), c.clone()], &local, &["mods/a.jar".into(), "mods/b.jar".into()], &HashMap::new());
        assert_eq!(plan.download, vec![b, c]);
        assert!(plan.delete.is_empty());
    }

    #[test]
    fn tamanho_diferente_forca_download() {
        let a = pf("mods/a.jar", "a");
        let local: HashMap<_, _> = [("mods/a.jar".to_string(), String::new())].into();
        assert_eq!(plan_sync(&[a.clone()], &local, &["mods/a.jar".into()], &HashMap::new()).download, vec![a]);
    }

    #[test]
    fn remove_mods_fora_do_manifest() {
        let a = pf("mods/a.jar", "a");
        let local = map(&[("mods/a.jar", &a)]);
        let disk = vec!["mods/a.jar".to_string(), "mods/hack.jar".to_string(), "mods/sub/extra.jar".to_string(), "mods/A.JAR.disabled".to_string()];
        let plan = plan_sync(&[a], &local, &disk, &HashMap::new());
        assert_eq!(plan.delete, vec!["mods/A.JAR.disabled", "mods/hack.jar", "mods/sub/extra.jar"]);
    }

    #[test]
    fn mods_comparacao_ignora_maiusculas_do_windows() {
        let a = pf("mods/Abc.jar", "a");
        let local = map(&[("mods/Abc.jar", &a)]);
        let plan = plan_sync(&[a], &local, &["mods/abc.jar".to_string()], &HashMap::new());
        assert!(plan.delete.is_empty());
    }

    #[test]
    fn preserva_part_de_download_a_retomar() {
        let a = pf("mods/a.jar", "a");
        let plan = plan_sync(&[a.clone()], &HashMap::new(), &["mods/a.jar.part".into(), "mods/lixo.jar.part".into()], &HashMap::new());
        assert_eq!(plan.delete, vec!["mods/lixo.jar.part"]);
        assert_eq!(plan.download, vec![a]);
    }

    #[test]
    fn fora_de_mods_so_remove_o_que_o_pack_anterior_instalou() {
        let keep = pf("config/keep.toml", "k");
        let prev: HashMap<String, String> = [
            ("config/old.toml".to_string(), "1".repeat(64)),
            ("config/keep.toml".to_string(), keep.sha256.clone()),
            ("resourcepacks/antigo.zip".to_string(), "2".repeat(64)),
            ("mods/removido.jar".to_string(), "3".repeat(64)),
            ("servers.dat".to_string(), "4".repeat(64)),
        ]
        .into();
        let local = map(&[("config/keep.toml", &keep)]);
        // resourcepack do próprio jogador (não está em prev) nunca é listado, logo nunca é apagado.
        let plan = plan_sync(&[keep], &local, &[], &prev);
        assert_eq!(plan.delete, vec!["config/old.toml", "resourcepacks/antigo.zip"]);
        assert!(plan.download.is_empty());
    }

    #[test]
    fn servers_dat_so_sobrescreve_quando_o_pack_muda() {
        let s = pf("servers.dat", "5");
        let local: HashMap<_, _> = [("servers.dat".to_string(), "9".repeat(64))].into(); // jogador editou
        // pack não mudou desde a última sync -> mantém o arquivo do jogador
        let prev: HashMap<_, _> = [("servers.dat".to_string(), s.sha256.clone())].into();
        assert!(plan_sync(&[s.clone()], &local, &[], &prev).download.is_empty());
        // pack mudou -> sobrescreve
        let prev2: HashMap<_, _> = [("servers.dat".to_string(), "6".repeat(64))].into();
        assert_eq!(plan_sync(&[s.clone()], &local, &[], &prev2).download.len(), 1);
        // não existe localmente -> baixa
        assert_eq!(plan_sync(&[s], &HashMap::new(), &[], &prev).download.len(), 1);
    }

    #[test]
    fn validate_path_bloqueia_traversal_e_afins() {
        for ok in ["mods/a.jar", "config/x/y.toml", "servers.dat", "mods/Mód 1.jar", "shaderpacks/s.zip"] {
            assert!(validate_path(ok).is_ok(), "{ok}");
        }
        for bad in ["../x", "mods/../../x", "/mods/a", "mods//a", "mods\\a", "C:/x", "mods/a:b", "mods/CON", "mods/nul.txt", "mods/com1", "mods/a.", "logs/x", "options.txt", "mods", "", "mods/./a", "mods/a\u{0}b"] {
            assert!(validate_path(bad).is_err(), "{bad:?}");
        }
        assert!(validate_path("mods/com0.jar").is_ok()); // COM0 não é reservado
    }

    #[test]
    fn validate_manifest_rejeita_dados_ruins() {
        let mut m = Manifest {
            pack_version: "1".into(),
            mc_version: "1.21.1".into(),
            neoforge_version: "21.1.172".into(),
            server_address: "x:25565".into(),
            min_ram_mb: 4096,
            news: String::new(),
            files: vec![pf("mods/a.jar", "a")],
        };
        assert!(validate_manifest(&m).is_ok());
        m.files.push(pf("mods/A.jar", "b"));
        assert!(validate_manifest(&m).is_err(), "duplicado case-insensitive");
        m.files.pop();
        m.files[0].url = "//evil.com/x".into();
        assert!(validate_manifest(&m).is_err());
        m.files[0].url = "ftp://x".into();
        assert!(validate_manifest(&m).is_err());
        m.files[0].url = "/files/a".into();
        m.files[0].sha256 = "zz".into();
        assert!(validate_manifest(&m).is_err());
        m.files[0].sha256 = "a".repeat(64);
        m.neoforge_version = "../../x".into();
        assert!(validate_manifest(&m).is_err());
    }

    #[test]
    fn resolve_url_relativa_e_absoluta() {
        assert_eq!(resolve_url("https://api.x.com/", "/files/mods/a%20b.jar"), "https://api.x.com/files/mods/a%20b.jar");
        assert_eq!(resolve_url("https://api.x.com", "https://cdn.y.com/a.jar"), "https://cdn.y.com/a.jar");
    }

    #[test]
    fn sync_integrado_em_disco_apaga_e_hasheia() {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path();
        std::fs::create_dir_all(game.join("mods")).unwrap();
        std::fs::write(game.join("mods/ok.jar"), b"0123456789").unwrap();
        std::fs::write(game.join("mods/extra.jar"), b"x").unwrap();
        let sha = sha256_file(&game.join("mods/ok.jar")).unwrap();
        let files = vec![PackFile { path: "mods/ok.jar".into(), sha256: sha, size: 10, url: "/f".into() }];
        let local = hash_local(game, &files);
        let mut managed = vec![];
        list_files(&game.join("mods"), "mods", &mut managed);
        let plan = plan_sync(&files, &local, &managed, &HashMap::new());
        assert!(plan.download.is_empty());
        assert_eq!(plan.delete, vec!["mods/extra.jar"]);
    }
}
