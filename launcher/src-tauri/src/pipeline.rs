//! Orquestra "JOGAR": manifest -> Java -> Minecraft -> NeoForge -> modpack -> comando -> execução.
use crate::config::AppConfig;
use crate::download;
use crate::error::{AppError, Result};
use crate::launch::{self, GameHandle};
use crate::pack::{self, Manifest};
use crate::report::{stage, Report, Reporter};
use crate::settings::{self, Dirs, Settings};
use crate::version::{self, Features, LaunchParams};
use crate::{java, mojang, neoforge, uuid};
use reqwest::Client;
use std::path::PathBuf;

pub struct Ctx {
    pub cfg: AppConfig,
    pub settings: Settings,
    pub client: Client,
    pub rep: Reporter,
    pub app_version: String,
}

pub struct Prepared {
    pub cmd: Vec<String>,
    pub game_dir: PathBuf,
    pub log_file: PathBuf,
    pub manifest: Manifest,
    pub offline: bool,
}

impl Ctx {
    pub fn dirs(&self) -> Dirs {
        Dirs::new(self.settings.root(&self.cfg))
    }
}

async fn ensure_dirs(dirs: &Dirs) -> Result<()> {
    for d in [dirs.cache(), dirs.state(), dirs.game(), dirs.runtime()] {
        tokio::fs::create_dir_all(&d).await.map_err(|e| AppError::from(e).context(format!("Não consegui criar a pasta {}", d.display())))?;
    }
    Ok(())
}

/// Obtém o manifest da API; se a API estiver fora do ar, usa o último salvo (modo offline).
pub async fn load_manifest(ctx: &Ctx, dirs: &Dirs) -> Result<(Manifest, bool)> {
    match pack::fetch_manifest(&ctx.client, &ctx.cfg.api_url).await {
        Ok(m) => {
            let _ = pack::save_manifest_cache(dirs, &m).await;
            Ok((m, false))
        }
        Err(e) => match pack::load_manifest_cache(dirs).await {
            Some(m) => {
                (ctx.rep)(Report::Warning {
                    message: "Não consegui falar com o servidor de atualizações. Vou abrir com a última versão instalada.".into(),
                });
                Ok((m, true))
            }
            None => Err(AppError::new(format!(
                "{} (A primeira instalação precisa de internet e do servidor de atualizações no ar.)",
                e.0
            ))),
        },
    }
}

/// Instala/atualiza tudo e monta o comando. `full` = reparar (revalida tudo).
pub async fn prepare(ctx: &Ctx, full: bool) -> Result<Prepared> {
    let s = &ctx.settings;
    if s.nickname.is_empty() {
        return Err(AppError::new("Digite seu nome de jogador antes de jogar."));
    }
    settings::validate_nickname(&s.nickname)?;
    let dirs = ctx.dirs();
    ensure_dirs(&dirs).await?;
    let rep = &ctx.rep;

    stage(rep, "Procurando atualizações...");
    let (manifest, offline) = load_manifest(ctx, &dirs).await?;

    let java_exe = java::ensure_java(&ctx.client, &dirs, rep).await?;

    let features = Features::new(); // regras de libraries não dependem de features
    let vanilla = mojang::ensure_version_json(&ctx.client, &dirs, &manifest.mc_version, full).await?;
    mojang::ensure_game(&ctx.client, &dirs, &vanilla, &features, full, rep).await?;
    let merged = neoforge::ensure(&ctx.client, &dirs, &java_exe, &vanilla, &manifest.neoforge_version, &features, full, rep).await?;

    if offline {
        pack::verify_installed_quick(&dirs, &manifest)?;
    } else {
        pack::sync(&ctx.client, &dirs, &ctx.cfg.api_url, &manifest, rep).await?;
    }

    stage(rep, "Iniciando o jogo...");
    let total = settings::total_ram_mb();
    let xmx = settings::effective_ram_mb(s.ram_mb, total, manifest.min_ram_mb);
    let params = LaunchParams {
        java: java_exe,
        root: dirs.root.clone(),
        game_dir: dirs.game(),
        username: s.nickname.clone(),
        uuid: uuid::offline_uuid_undashed(&s.nickname),
        xmx_mb: xmx,
        xms_mb: if xmx >= 2048 { 1024 } else { 512 },
        extra_jvm: version::split_args(&s.jvm_args),
        resolution: (s.width > 0 && s.height > 0).then_some((s.width, s.height)),
        quick_play: s.quick_play.then(|| manifest.server_address.clone()),
        launcher_name: ctx.cfg.name.clone(),
        launcher_version: ctx.app_version.clone(),
        version_name: merged.id.clone(),
        jar_version: vanilla.id.clone(),
    };
    let feats = version::features_for(&params);
    let cp = version::classpath(&merged, &feats, &dirs.libraries(), &mojang::client_jar(&dirs, &vanilla.id));
    let natives = dirs.version_dir(&merged.id).join("natives");
    tokio::fs::create_dir_all(&natives).await?;
    let cmd = version::build_command(&merged, &params, &cp, &natives, &dirs.assets())?;

    Ok(Prepared { cmd, game_dir: dirs.game(), log_file: dirs.root.join("logs").join("ultimo-jogo.log"), manifest, offline })
}

/// "JOGAR": prepara e abre o jogo.
pub async fn play(ctx: &Ctx, full: bool) -> Result<(GameHandle, Prepared)> {
    let p = prepare(ctx, full).await?;
    let h = launch::launch(p.cmd.clone(), p.game_dir.clone(), p.log_file.clone(), ctx.rep.clone()).await?;
    Ok((h, p))
}

/// Limpa arquivos temporários (instaladores e zips baixados).
pub async fn clear_cache(dirs: &Dirs) -> Result<u64> {
    let mut freed = 0u64;
    if let Ok(mut rd) = tokio::fs::read_dir(dirs.cache()).await {
        while let Ok(Some(e)) = rd.next_entry().await {
            if let Ok(m) = e.metadata().await {
                freed += m.len();
            }
            let p = e.path();
            let _ = if p.is_dir() { tokio::fs::remove_dir_all(&p).await } else { tokio::fs::remove_file(&p).await };
        }
    }
    Ok(freed)
}

pub fn client_for_status() -> Result<Client> {
    download::build_client()
}
