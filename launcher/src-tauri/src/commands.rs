//! Comandos expostos ao frontend (tauri::command). Toda a lógica real fica nos outros módulos.
use crate::config::AppConfig;
use crate::error::{AppError, Result};
use crate::launch::GameHandle;
use crate::pack;
use crate::pipeline::{self, Ctx};
use crate::report::{Report, Reporter};
use crate::settings::{self, Settings, DEFAULT_JVM_ARGS};
use crate::status::{self, ServerStatus};
use reqwest::Client;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_updater::UpdaterExt;
use tokio::sync::oneshot;

pub struct AppState {
    pub cfg: AppConfig,
    pub client: Client,
    pub settings: Mutex<Settings>,
    /// true enquanto instala/repara/joga (evita duas operações ao mesmo tempo).
    pub busy: Arc<AtomicBool>,
    pub kill: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

/// Libera o `busy` ao sair do escopo, mesmo em erro/pânico.
struct BusyGuard(Arc<AtomicBool>);
impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

fn acquire(state: &AppState) -> Result<BusyGuard> {
    if state.busy.swap(true, Ordering::SeqCst) {
        return Err(AppError::new("Já existe uma operação em andamento. Aguarde terminar."));
    }
    Ok(BusyGuard(state.busy.clone()))
}

fn reporter(app: &AppHandle) -> Reporter {
    let app = app.clone();
    Arc::new(move |r: Report| {
        let _ = app.emit("launcher-event", r);
    })
}

fn make_ctx(app: &AppHandle, state: &AppState) -> Ctx {
    Ctx {
        cfg: state.cfg.clone(),
        settings: state.settings.lock().unwrap().clone(),
        client: state.client.clone(),
        rep: reporter(app),
        app_version: app.package_info().version.to_string(),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitInfo {
    name: String,
    version: String,
    settings: Settings,
    total_ram_mb: u64,
    default_ram_mb: u32,
    default_jvm_args: String,
    game_root: String,
}

#[tauri::command]
pub fn get_init(app: AppHandle, state: State<'_, AppState>) -> InitInfo {
    let s = state.settings.lock().unwrap().clone();
    let total = settings::total_ram_mb();
    InitInfo {
        name: state.cfg.name.clone(),
        version: app.package_info().version.to_string(),
        game_root: s.root(&state.cfg).to_string_lossy().into_owned(),
        settings: s,
        total_ram_mb: total,
        default_ram_mb: settings::smart_ram_mb(total),
        default_jvm_args: DEFAULT_JVM_ARGS.to_string(),
    }
}

#[tauri::command]
pub fn save_settings(new: Settings, state: State<'_, AppState>) -> Result<()> {
    new.validate(settings::total_ram_mb())?;
    new.save(&state.cfg)?;
    *state.settings.lock().unwrap() = new;
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackInfo {
    pack_version: String,
    news: String,
    server_address: String,
    min_ram_mb: u32,
    /// true = a API não respondeu; os dados vêm da última versão instalada.
    offline: bool,
}

/// Dados da tela inicial: notícias, IP do servidor etc. Se a API falhar e não houver cópia salva,
/// devolve o erro (ex: "sem pack publicado" vs. "sem conexão") para a UI explicar o motivo certo.
#[tauri::command]
pub async fn refresh_info(state: State<'_, AppState>) -> Result<Option<PackInfo>> {
    let s = state.settings.lock().unwrap().clone();
    let dirs = crate::settings::Dirs::new(s.root(&state.cfg));
    let (m, offline) = match pack::fetch_manifest(&state.client, &state.cfg.api_url).await {
        Ok(m) => {
            let _ = pack::save_manifest_cache(&dirs, &m).await;
            (m, false)
        }
        Err(e) => match pack::load_manifest_cache(&dirs).await {
            Some(m) => (m, true),
            None => return Err(e),
        },
    };
    Ok(Some(PackInfo { pack_version: m.pack_version, news: m.news, server_address: m.server_address, min_ram_mb: m.min_ram_mb, offline }))
}

#[tauri::command]
pub async fn server_status(address: String) -> ServerStatus {
    status::ping(&address).await
}

/// JOGAR: atualiza tudo e abre o jogo. Retorna quando o jogo foi iniciado; o fim vem em "launcher-event".
#[tauri::command]
pub async fn play(app: AppHandle, state: State<'_, AppState>) -> Result<()> {
    let guard = acquire(&state)?;
    let ctx = make_ctx(&app, &state);
    let (handle, _) = pipeline::play(&ctx, false).await?;
    let GameHandle { task, kill } = handle;
    *state.kill.lock().unwrap() = kill;
    // Mantém o "ocupado" até o jogo fechar (impede abrir duas instâncias / reparar com o jogo aberto).
    let kill_slot = state.kill.clone();
    tokio::spawn(async move {
        let _guard = guard;
        let _ = task.await;
        *kill_slot.lock().unwrap() = None;
    });
    Ok(())
}

#[tauri::command]
pub fn stop_game(state: State<'_, AppState>) {
    if let Some(k) = state.kill.lock().unwrap().take() {
        let _ = k.send(());
    }
}

/// Reparar instalação: revalida e reinstala tudo (Java, Minecraft, NeoForge, modpack).
#[tauri::command]
pub async fn repair(app: AppHandle, state: State<'_, AppState>) -> Result<()> {
    let _guard = acquire(&state)?;
    let ctx = make_ctx(&app, &state);
    // O nick pode estar vazio ao reparar; usa um provisório só para satisfazer a validação.
    let mut ctx = ctx;
    if ctx.settings.nickname.is_empty() {
        ctx.settings.nickname = "Jogador".into();
    }
    pipeline::prepare(&ctx, true).await?;
    Ok(())
}

/// Limpa instaladores/zip temporários. Retorna bytes liberados.
#[tauri::command]
pub async fn clear_cache(state: State<'_, AppState>) -> Result<u64> {
    let _guard = acquire(&state)?;
    let s = state.settings.lock().unwrap().clone();
    pipeline::clear_cache(&crate::settings::Dirs::new(s.root(&state.cfg))).await
}

fn open_path_with_os(target: &str) -> std::io::Result<()> {
    #[cfg(windows)]
    let mut c = std::process::Command::new("explorer");
    #[cfg(target_os = "macos")]
    let mut c = std::process::Command::new("open");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut c = std::process::Command::new("xdg-open");
    c.arg(target).spawn().map(|_| ())
}

#[tauri::command]
pub fn open_game_dir(state: State<'_, AppState>) -> Result<()> {
    let s = state.settings.lock().unwrap().clone();
    let dir = crate::settings::Dirs::new(s.root(&state.cfg)).game();
    std::fs::create_dir_all(&dir)?;
    open_path_with_os(&dir.to_string_lossy())?;
    Ok(())
}

/// Abre um link http(s) no navegador padrão (links das notícias).
#[tauri::command]
pub fn open_url(url: String) -> Result<()> {
    let ok = (url.starts_with("https://") || url.starts_with("http://")) && !url.chars().any(|c| c.is_whitespace() || c.is_control());
    if !ok {
        return Err(AppError::new("Link inválido."));
    }
    open_path_with_os(&url)?;
    Ok(())
}

// ---------- auto-update do launcher (updater oficial do Tauri; pacote assinado) ----------

#[derive(Serialize)]
pub struct UpdateInfo {
    version: String,
    notes: String,
}

async fn updater(app: &AppHandle, state: &AppState) -> Result<tauri_plugin_updater::Updater> {
    let url = format!("{}/api/launcher/version", state.cfg.api_url);
    let url: tauri::Url = url.parse().map_err(|_| AppError::new("URL da API inválida no launcher.config.json."))?;
    app.updater_builder()
        .endpoints(vec![url])
        .map_err(|e| AppError::new(format!("Updater: {e}")))?
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| AppError::new(format!("Updater: {e}")))
}

/// Há versão nova do launcher? Falhas de rede viram `None` (não atrapalha quem está offline).
#[tauri::command]
pub async fn check_update(app: AppHandle, state: State<'_, AppState>) -> Result<Option<UpdateInfo>> {
    let u = updater(&app, &state).await?;
    match u.check().await {
        Ok(Some(up)) => Ok(Some(UpdateInfo { version: up.version.clone(), notes: up.body.clone().unwrap_or_default() })),
        Ok(None) | Err(_) => Ok(None),
    }
}

/// Baixa, verifica a assinatura e instala. No Windows o app fecha e o instalador reabre o launcher.
#[tauri::command]
pub async fn install_update(app: AppHandle, state: State<'_, AppState>) -> Result<()> {
    let _guard = acquire(&state)?;
    let u = updater(&app, &state).await?;
    let up = u
        .check()
        .await
        .map_err(|e| AppError::new(format!("Não consegui verificar a atualização: {e}")))?
        .ok_or_else(|| AppError::new("Nenhuma atualização disponível."))?;
    let rep = reporter(&app);
    let mut done: u64 = 0;
    up.download_and_install(
        |chunk, total| {
            done += chunk as u64;
            rep(Report::Progress {
                label: "Atualizando o launcher".into(),
                done_bytes: done,
                total_bytes: total.unwrap_or(0),
                files_done: 0,
                files_total: 1,
                speed_bps: 0,
                eta_secs: None,
            });
        },
        || {},
    )
    .await
    .map_err(|e| AppError::new(format!("Falha ao atualizar o launcher (assinatura inválida ou download interrompido): {e}")))?;
    app.restart();
}
