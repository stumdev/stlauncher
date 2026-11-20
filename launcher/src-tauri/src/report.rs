//! Eventos enviados do núcleo para a UI (um único canal "launcher-event").
use serde::Serialize;
use std::sync::Arc;

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Report {
    /// Nova etapa (texto para mostrar ao usuário).
    Stage { label: String },
    Progress {
        label: String,
        done_bytes: u64,
        total_bytes: u64,
        files_done: u64,
        files_total: u64,
        speed_bps: u64,
        eta_secs: Option<u64>,
    },
    /// Aviso não fatal (ex: modo offline).
    Warning { message: String },
    /// Linha de log do jogo (ou do instalador).
    Log { line: String, stream: String },
    GameStarted,
    GameExit {
        code: Option<i32>,
        crashed: bool,
        summary: Option<String>,
        crash_report: Option<String>,
    },
}

pub type Reporter = Arc<dyn Fn(Report) + Send + Sync>;

pub fn stage(r: &Reporter, label: impl Into<String>) {
    r(Report::Stage { label: label.into() });
}
