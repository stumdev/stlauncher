//! Java 21: detecção de instalações existentes e download automático do JRE Temurin 21.
use crate::download::{self, Hash, Task};
use crate::error::{AppError, Result};
use crate::report::{stage, Reporter};
use crate::settings::Dirs;
use reqwest::Client;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

pub const REQUIRED_MAJOR: u32 = 21;

/// Evita abrir janela de console ao executar processos no Windows.
pub fn hide_window(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

fn exe_name() -> &'static str {
    if cfg!(windows) { "java.exe" } else { "java" }
}

pub fn bundled_java(dirs: &Dirs) -> PathBuf {
    dirs.runtime().join("jre-21").join("bin").join(exe_name())
}

/// Extrai a versão principal de uma saída de `java -version`.
/// `openjdk version "21.0.5" ...` -> 21 ; `"1.8.0_392"` -> 8 ; `"25"` -> 25
pub fn parse_major(output: &str) -> Option<u32> {
    let line = output.lines().find(|l| l.contains(" version \""))?;
    let v = line.split('"').nth(1)?;
    let mut parts = v.split(|c: char| c == '.' || c == '_' || c == '-' || c == '+');
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 {
        parts.next()?.parse().ok()
    } else {
        Some(first)
    }
}

/// Executa `java -version` e devolve a versão principal.
pub async fn probe(java: &Path) -> Option<u32> {
    let mut cmd = Command::new(java);
    cmd.arg("-version").kill_on_drop(true);
    hide_window(&mut cmd);
    let out = tokio::time::timeout(Duration::from_secs(15), cmd.output()).await.ok()?.ok()?;
    parse_major(&String::from_utf8_lossy(&out.stderr)).or_else(|| parse_major(&String::from_utf8_lossy(&out.stdout)))
}

/// Candidatos onde um Java 21 pode estar (em ordem de preferência).
fn candidates(dirs: &Dirs) -> Vec<PathBuf> {
    let mut c = vec![bundled_java(dirs)];
    if let Ok(h) = std::env::var("JAVA_HOME") {
        c.push(PathBuf::from(h).join("bin").join(exe_name()));
    }
    #[cfg(windows)]
    {
        let mut bases: Vec<PathBuf> = Vec::new();
        for var in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Ok(pf) = std::env::var(var) {
                for sub in ["Eclipse Adoptium", "Java", "Microsoft", "Zulu", "BellSoft", "Amazon Corretto", "Semeru", "Eclipse Foundation"] {
                    bases.push(PathBuf::from(&pf).join(sub));
                }
            }
        }
        for b in bases {
            if let Ok(rd) = std::fs::read_dir(&b) {
                for e in rd.flatten() {
                    if e.file_name().to_string_lossy().contains("21") {
                        c.push(e.path().join("bin").join(exe_name()));
                    }
                }
            }
        }
    }
    c.push(PathBuf::from(exe_name())); // java no PATH
    c.retain(|p| p.components().count() == 1 || p.exists());
    c
}

/// Procura um Java 21 já instalado (o do launcher vem primeiro).
pub async fn find_java21(dirs: &Dirs) -> Option<PathBuf> {
    for c in candidates(dirs) {
        if probe(&c).await == Some(REQUIRED_MAJOR) {
            return Some(c);
        }
    }
    None
}

// ---------- download do Temurin ----------

#[derive(Deserialize)]
struct Asset {
    binary: Binary,
}
#[derive(Deserialize)]
struct Binary {
    package: Package,
}
#[derive(Deserialize)]
struct Package {
    link: String,
    checksum: String,
    size: u64,
    name: String,
}

#[cfg(windows)]
const ADOPTIUM_API: &str = "https://api.adoptium.net/v3/assets/latest/21/hotspot?architecture=x64&image_type=jre&os=windows&vendor=eclipse";

/// Baixa e instala o JRE Temurin 21 em <raiz>/runtime/jre-21. Devolve o caminho do java.
pub async fn install_temurin(client: &Client, dirs: &Dirs, rep: &Reporter) -> Result<PathBuf> {
    #[cfg(not(windows))]
    {
        let _ = (client, rep);
        let _ = dirs;
        Err(AppError::new("Instale o Java 21 (Temurin) manualmente: o download automático só está implementado para Windows."))
    }
    #[cfg(windows)]
    {
        stage(rep, "Preparando o Java 21 (só na primeira vez)...");
        let assets: Vec<Asset> = download::get_json(client, ADOPTIUM_API, Duration::from_secs(20)).await?;
        let pkg = assets
            .into_iter()
            .next()
            .map(|a| a.binary.package)
            .ok_or_else(|| AppError::new("Não encontrei o Java 21 para download (resposta vazia do Adoptium)."))?;
        if !pkg.name.ends_with(".zip") {
            return Err(AppError::new("Formato inesperado do pacote Java (esperava .zip)."));
        }

        let zip_path = dirs.cache().join(&pkg.name);
        let task = Task { url: pkg.link, dest: zip_path.clone(), hash: Hash::Sha256(pkg.checksum), size: pkg.size };
        download::run_downloads(client, vec![task], 1, "Java 21", rep).await?;

        stage(rep, "Instalando o Java 21...");
        let target = dirs.runtime().join("jre-21");
        let tmp = dirs.runtime().join(".jre-21.tmp");
        let (z, t, tg) = (zip_path.clone(), tmp.clone(), target.clone());
        tokio::task::spawn_blocking(move || -> Result<()> {
            let _ = std::fs::remove_dir_all(&t);
            std::fs::create_dir_all(&t)?;
            extract_zip_strip_top(&z, &t)?;
            let _ = std::fs::remove_dir_all(&tg);
            std::fs::rename(&t, &tg)?;
            Ok(())
        })
        .await??;
        let _ = tokio::fs::remove_file(&zip_path).await;

        let java = bundled_java(dirs);
        if probe(&java).await != Some(REQUIRED_MAJOR) {
            return Err(AppError::new("O Java 21 foi instalado mas não executa. O antivírus pode ter bloqueado o java.exe; adicione a pasta do launcher às exceções e use \"Reparar instalação\"."));
        }
        Ok(java)
    }
}

/// Extrai um zip removendo o diretório de topo (ex: jdk-21.0.5+11-jre/). Protege contra zip-slip.
pub fn extract_zip_strip_top(zip_path: &Path, dest: &Path) -> Result<()> {
    let mut ar = zip::ZipArchive::new(std::fs::File::open(zip_path)?)?;
    for i in 0..ar.len() {
        let mut f = ar.by_index(i)?;
        let Some(rel) = f.enclosed_name() else {
            return Err(AppError::new("O pacote do Java contém um caminho inválido (arquivo suspeito)."));
        };
        let stripped: PathBuf = rel.components().skip(1).collect();
        if stripped.as_os_str().is_empty() {
            continue;
        }
        let out = dest.join(stripped);
        if f.is_dir() {
            std::fs::create_dir_all(&out)?;
        } else {
            if let Some(p) = out.parent() {
                std::fs::create_dir_all(p)?;
            }
            let mut w = std::fs::File::create(&out)?;
            std::io::copy(&mut f, &mut w)?;
        }
    }
    Ok(())
}

/// Garante um Java 21 utilizável (detecta ou baixa).
pub async fn ensure_java(client: &Client, dirs: &Dirs, rep: &Reporter) -> Result<PathBuf> {
    stage(rep, "Procurando o Java 21...");
    if let Some(j) = find_java21(dirs).await {
        return Ok(j);
    }
    install_temurin(client, dirs, rep).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parse_de_versoes() {
        assert_eq!(parse_major("openjdk version \"21.0.5\" 2024-10-15 LTS\nOpenJDK Runtime"), Some(21));
        assert_eq!(parse_major("openjdk version \"25.0.5\" 2026-10-20 LTS"), Some(25));
        assert_eq!(parse_major("java version \"1.8.0_392\"\nJava(TM) SE"), Some(8));
        assert_eq!(parse_major("openjdk version \"17\" 2021-09-14"), Some(17));
        assert_eq!(parse_major("openjdk version \"21-ea\" 2023-09-19"), Some(21));
        assert_eq!(parse_major("não é java"), None);
    }

    #[test]
    fn extrai_zip_removendo_pasta_de_topo() {
        let dir = tempfile::tempdir().unwrap();
        let zp = dir.path().join("j.zip");
        {
            let mut z = zip::ZipWriter::new(std::fs::File::create(&zp).unwrap());
            let o = zip::write::SimpleFileOptions::default();
            z.add_directory("jdk-21-jre/", o).unwrap();
            z.start_file("jdk-21-jre/bin/java.exe", o).unwrap();
            z.write_all(b"EXE").unwrap();
            z.start_file("jdk-21-jre/release", o).unwrap();
            z.write_all(b"R").unwrap();
            z.finish().unwrap();
        }
        let out = dir.path().join("out");
        extract_zip_strip_top(&zp, &out).unwrap();
        assert_eq!(std::fs::read(out.join("bin").join("java.exe")).unwrap(), b"EXE");
        assert_eq!(std::fs::read(out.join("release")).unwrap(), b"R");
    }

    #[test]
    fn zip_slip_e_rejeitado() {
        let dir = tempfile::tempdir().unwrap();
        let zp = dir.path().join("evil.zip");
        {
            let mut z = zip::ZipWriter::new(std::fs::File::create(&zp).unwrap());
            let o = zip::write::SimpleFileOptions::default();
            z.start_file("jdk/../../escapou.txt", o).unwrap();
            z.write_all(b"x").unwrap();
            z.finish().unwrap();
        }
        let out = dir.path().join("out");
        assert!(extract_zip_strip_top(&zp, &out).is_err());
        assert!(!dir.path().join("escapou.txt").exists());
    }
}

#[cfg(all(test, windows))]
mod net_tests {
    use super::*;

    /// Teste com rede real (baixa ~50 MB do Adoptium). Rode com:
    /// cargo test --lib temurin -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn temurin_baixa_instala_e_executa() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = Dirs::new(dir.path().to_path_buf());
        let rep: Reporter = std::sync::Arc::new(|_| {});
        let j = install_temurin(&crate::download::build_client().unwrap(), &dirs, &rep).await.unwrap();
        assert_eq!(j, bundled_java(&dirs));
        assert_eq!(probe(&j).await, Some(21));
        assert_eq!(find_java21(&dirs).await.as_deref(), Some(j.as_path())); // o do launcher tem prioridade
    }
}
