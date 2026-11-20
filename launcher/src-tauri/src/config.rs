//! Configuração embutida no build (launcher/launcher.config.json): nome do launcher e URL da API.
use serde::{Deserialize, Serialize};

const RAW: &str = include_str!("../../launcher.config.json");

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    /// Nome exibido na janela e usado na pasta de dados.
    pub name: String,
    /// URL base da API (sem barra final), ex: https://api.meusite.com
    pub api_url: String,
}

impl AppConfig {
    pub fn load() -> AppConfig {
        let mut c: AppConfig = serde_json::from_str(RAW).expect("launcher.config.json inválido");
        c.api_url = c.api_url.trim_end_matches('/').to_string();
        c
    }

    /// Nome seguro para usar como nome de pasta no Windows.
    pub fn folder_name(&self) -> String {
        let s: String = self
            .name
            .chars()
            .map(|c| if "<>:\"/\\|?*".contains(c) || c.is_control() { '_' } else { c })
            .collect();
        let s = s.trim().trim_end_matches('.').to_string();
        if s.is_empty() { "STLauncher".into() } else { s }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn config_embutida_valida() {
        let c = AppConfig::load();
        assert!(!c.name.is_empty());
        assert!(c.api_url.starts_with("http"));
        assert!(!c.api_url.ends_with('/'));
    }
    #[test]
    fn folder_name_sanitiza() {
        let c = AppConfig { name: "Meu: Launcher?".into(), api_url: "http://x".into() };
        assert_eq!(c.folder_name(), "Meu_ Launcher_");
    }
}
