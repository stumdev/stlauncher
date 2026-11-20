//! Erro único do launcher: carrega SEMPRE uma mensagem pronta em português para o usuário.
use serde::{Serialize, Serializer};
use std::fmt;

#[derive(Debug, Clone)]
pub struct AppError(pub String);
pub type Result<T> = std::result::Result<T, AppError>;

impl AppError {
    pub fn new(msg: impl Into<String>) -> Self {
        AppError(msg.into())
    }
    /// Prefixa a mensagem com o contexto do que estava sendo feito.
    pub fn context(self, ctx: impl fmt::Display) -> Self {
        AppError(format!("{ctx}: {}", self.0))
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for AppError {}

// O frontend recebe só a string.
impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

/// Códigos de "disco cheio": Windows ERROR_DISK_FULL(112)/ERROR_HANDLE_DISK_FULL(39); Unix ENOSPC(28).
pub fn is_disk_full(e: &std::io::Error) -> bool {
    match e.raw_os_error() {
        #[cfg(windows)]
        Some(112) | Some(39) => true,
        #[cfg(not(windows))]
        Some(28) => true,
        _ => false,
    }
}

/// Erros de IO que não adianta tentar de novo (disco cheio, permissão, caminho inválido).
pub fn is_fatal_io(e: &std::io::Error) -> bool {
    is_disk_full(e) || e.kind() == std::io::ErrorKind::PermissionDenied || matches!(e.raw_os_error(), Some(5) | Some(123) | Some(206))
}

pub fn io_message(e: &std::io::Error) -> String {
    if is_disk_full(e) {
        return "Sem espaço em disco. Libere espaço (o jogo precisa de alguns GB) ou escolha outra pasta nas Configurações.".into();
    }
    match e.raw_os_error() {
        #[cfg(windows)]
        Some(32) | Some(33) | Some(1224) => {
            return "Um arquivo está em uso por outro programa. Feche o Minecraft (e outros launchers) e tente de novo. Se continuar, o antivírus pode estar verificando o arquivo.".into();
        }
        #[cfg(windows)]
        Some(206) => return "O caminho da pasta do jogo é longo demais para o Windows. Escolha uma pasta mais curta nas Configurações.".into(),
        _ => {}
    }
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        return "Acesso negado ao gravar arquivos. O antivírus pode estar bloqueando o launcher, ou a pasta exige permissão de administrador. Adicione a pasta do launcher às exceções do antivírus ou escolha outra pasta nas Configurações.".into();
    }
    format!("Erro de arquivo: {e}")
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError(io_message(&e))
    }
}

pub fn reqwest_message(e: &reqwest::Error) -> String {
    let host = e.url().and_then(|u| u.host_str()).unwrap_or("servidor");
    // Procura sinais de falha TLS na cadeia de erros (comum com antivírus que intercepta HTTPS).
    let mut chain = String::new();
    let mut src: Option<&dyn std::error::Error> = Some(e);
    while let Some(s) = src {
        chain.push_str(&s.to_string().to_lowercase());
        chain.push(' ');
        src = s.source();
    }
    if chain.contains("certificate") || chain.contains("handshake") || chain.contains("tls") {
        return format!("Falha na conexão segura (HTTPS) com {host}. O antivírus/firewall pode estar interceptando a conexão, ou a data/hora do PC está errada.");
    }
    if e.is_timeout() {
        return format!("A conexão com {host} demorou demais. Verifique sua internet e tente de novo.");
    }
    if e.is_connect() {
        return format!("Não foi possível conectar a {host}. Verifique sua internet (ou se o servidor está no ar).");
    }
    if let Some(s) = e.status() {
        return match s.as_u16() {
            404 => format!("Arquivo não encontrado no servidor ({host})."),
            c if c >= 500 => format!("O servidor {host} está com problemas (HTTP {c}). Tente de novo mais tarde."),
            c => format!("O servidor {host} recusou o pedido (HTTP {c})."),
        };
    }
    if e.is_decode() || e.is_body() {
        return format!("Resposta inválida ou interrompida de {host}. Tente de novo.");
    }
    format!("Erro de rede com {host}: {e}")
}

impl From<reqwest::Error> for AppError {
    fn from(e: reqwest::Error) -> Self {
        AppError(reqwest_message(&e))
    }
}
impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError(format!("Dados em formato inesperado: {e}"))
    }
}
impl From<tokio::task::JoinError> for AppError {
    fn from(e: tokio::task::JoinError) -> Self {
        AppError(format!("Falha interna: {e}"))
    }
}
impl From<zip::result::ZipError> for AppError {
    fn from(e: zip::result::ZipError) -> Self {
        AppError(format!("Arquivo ZIP inválido ou corrompido: {e}"))
    }
}
