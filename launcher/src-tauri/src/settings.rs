//! Configurações do usuário, pastas e RAM.
use crate::config::AppConfig;
use crate::error::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Flags de JVM padrão: G1GC afinado (baseado nas "Aikar flags", sem AlwaysPreTouch para abrir rápido).
pub const DEFAULT_JVM_ARGS: &str = "-XX:+UseG1GC -XX:+ParallelRefProcEnabled -XX:MaxGCPauseMillis=200 \
-XX:+UnlockExperimentalVMOptions -XX:+DisableExplicitGC -XX:G1NewSizePercent=30 -XX:G1MaxNewSizePercent=40 \
-XX:G1HeapRegionSize=8M -XX:G1ReservePercent=20 -XX:G1HeapWastePercent=5 -XX:G1MixedGCCountTarget=4 \
-XX:InitiatingHeapOccupancyPercent=15 -XX:G1MixedGCLiveThresholdPercent=90 -XX:G1RSetUpdatingPauseTimePercent=5 \
-XX:SurvivorRatio=32 -XX:+PerfDisableSharedMem -XX:MaxTenuringThreshold=1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub nickname: String,
    /// 0 = automático (padrão inteligente).
    pub ram_mb: u32,
    /// Vazio = pasta padrão.
    pub game_dir: String,
    pub jvm_args: String,
    /// 0 = padrão do jogo.
    pub width: u32,
    pub height: u32,
    /// Conectar direto ao servidor ao abrir o jogo.
    pub quick_play: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            nickname: String::new(),
            ram_mb: 0,
            game_dir: String::new(),
            jvm_args: DEFAULT_JVM_ARGS.to_string(),
            width: 0,
            height: 0,
            quick_play: true,
        }
    }
}

/// Nick válido do Minecraft: 3-16 caracteres, letras/números/underscore.
pub fn validate_nickname(n: &str) -> Result<()> {
    let ok = (3..=16).contains(&n.chars().count()) && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if ok {
        Ok(())
    } else {
        Err(AppError::new("Nome inválido: use de 3 a 16 caracteres, apenas letras (sem acento), números e _ (sem espaços)."))
    }
}

/// Pasta fixa do launcher (onde fica settings.json), independente da pasta do jogo.
pub fn app_dir(cfg: &AppConfig) -> PathBuf {
    dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join(cfg.folder_name())
}

impl Settings {
    pub fn load(cfg: &AppConfig) -> Settings {
        let p = app_dir(cfg).join("settings.json");
        std::fs::read(&p).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, cfg: &AppConfig) -> Result<()> {
        let dir = app_dir(cfg);
        std::fs::create_dir_all(&dir)?;
        let tmp = dir.join("settings.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, dir.join("settings.json"))?;
        Ok(())
    }

    /// Pasta raiz do jogo (libraries, versions, assets, runtime, game...).
    pub fn root(&self, cfg: &AppConfig) -> PathBuf {
        if self.game_dir.trim().is_empty() {
            app_dir(cfg).join("data")
        } else {
            PathBuf::from(self.game_dir.trim())
        }
    }

    pub fn validate(&self, total_ram_mb: u64) -> Result<()> {
        if !self.nickname.is_empty() {
            validate_nickname(&self.nickname)?;
        }
        if self.ram_mb != 0 && (self.ram_mb < 1024 || u64::from(self.ram_mb) > total_ram_mb.max(2048)) {
            return Err(AppError::new("Valor de RAM inválido para este computador."));
        }
        if (self.width != 0 || self.height != 0) && !(self.width >= 640 && self.height >= 480 && self.width <= 16384 && self.height <= 16384) {
            return Err(AppError::new("Resolução inválida (mínimo 640x480)."));
        }
        if !self.game_dir.trim().is_empty() {
            let p = Path::new(self.game_dir.trim());
            if !p.is_absolute() {
                return Err(AppError::new("A pasta do jogo precisa ser um caminho completo (ex: D:\\Jogos\\Minecraft)."));
            }
        }
        Ok(())
    }
}

/// Estrutura de pastas dentro da raiz.
#[derive(Debug, Clone)]
pub struct Dirs {
    pub root: PathBuf,
}
impl Dirs {
    pub fn new(root: PathBuf) -> Self {
        Dirs { root }
    }
    pub fn versions(&self) -> PathBuf { self.root.join("versions") }
    pub fn libraries(&self) -> PathBuf { self.root.join("libraries") }
    pub fn assets(&self) -> PathBuf { self.root.join("assets") }
    pub fn runtime(&self) -> PathBuf { self.root.join("runtime") }
    /// Diretório do jogo (mods, config, saves...).
    pub fn game(&self) -> PathBuf { self.root.join("game") }
    pub fn cache(&self) -> PathBuf { self.root.join("cache") }
    pub fn state(&self) -> PathBuf { self.root.join("state") }
    pub fn version_dir(&self, id: &str) -> PathBuf { self.versions().join(id) }
}

/// RAM total do PC em MB.
pub fn total_ram_mb() -> u64 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        // SAFETY: estrutura zerada com dwLength preenchido, como exige a API.
        unsafe {
            let mut st: MEMORYSTATUSEX = std::mem::zeroed();
            st.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
            if GlobalMemoryStatusEx(&mut st) != 0 {
                return st.ullTotalPhys / (1024 * 1024);
            }
        }
        8192
    }
    #[cfg(not(windows))]
    {
        std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|s| s.lines().find(|l| l.starts_with("MemTotal:")).and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok()))
            .map(|kb| kb / 1024)
            .unwrap_or(8192)
    }
}

/// Padrão inteligente: 50% da RAM, entre 2 GB e 8 GB, arredondado para 256 MB.
pub fn smart_ram_mb(total_mb: u64) -> u32 {
    let half = (total_mb / 2).clamp(2048, 8192);
    ((half / 256) * 256) as u32
}

/// RAM efetiva: valor manual, ou (automático) o padrão inteligente elevado até o mínimo do pack
/// (sem passar de 75% da RAM do PC).
pub fn effective_ram_mb(setting: u32, total_mb: u64, pack_min_mb: u32) -> u32 {
    if setting != 0 {
        return setting;
    }
    let smart = smart_ram_mb(total_mb);
    let cap = ((total_mb * 3 / 4) as u32).max(2048);
    smart.max(pack_min_mb.min(cap))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nick_valido_e_invalido() {
        for ok in ["Bru", "Jogador_01", "a".repeat(16).as_str()] {
            assert!(validate_nickname(ok).is_ok(), "{ok}");
        }
        for bad in ["ab", "a".repeat(17).as_str(), "João", "com espaço", "x-y", "", "nick!"] {
            assert!(validate_nickname(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn ram_inteligente() {
        assert_eq!(smart_ram_mb(16384), 8192);
        assert_eq!(smart_ram_mb(32768), 8192); // limitado a 8GB
        assert_eq!(smart_ram_mb(8192), 4096);
        assert_eq!(smart_ram_mb(4096), 2048);
        assert_eq!(smart_ram_mb(2048), 2048); // piso
    }

    #[test]
    fn ram_efetiva_respeita_minimo_do_pack() {
        assert_eq!(effective_ram_mb(0, 8192, 6144), 6144); // 8GB PC, pack pede 6GB, cabe em 75%
        assert_eq!(effective_ram_mb(0, 4096, 6144), 3072); // limitado a 75% do PC
        assert_eq!(effective_ram_mb(0, 16384, 4096), 8192);
        assert_eq!(effective_ram_mb(3000, 16384, 6144), 3000); // manual manda
    }

    #[test]
    fn validacao_de_settings() {
        let mut s = Settings::default();
        assert!(s.validate(16384).is_ok());
        s.ram_mb = 512;
        assert!(s.validate(16384).is_err());
        s.ram_mb = 4096;
        s.width = 800;
        assert!(s.validate(16384).is_err()); // altura faltando
        s.height = 600;
        s.game_dir = "relativo/pasta".into();
        assert!(s.validate(16384).is_err());
    }

    #[test]
    fn json_parcial_usa_padroes() {
        let s: Settings = serde_json::from_str(r#"{"nickname":"Abc"}"#).unwrap();
        assert!(s.quick_play);
        assert_eq!(s.jvm_args, DEFAULT_JVM_ARGS);
    }
}
