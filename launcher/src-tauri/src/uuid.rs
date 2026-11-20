//! UUID offline, idêntico ao do servidor em online-mode=false:
//! `UUID.nameUUIDFromBytes(("OfflinePlayer:" + nome).getBytes(UTF_8))` (MD5 + bits de versão 3).
use md5::{Digest, Md5};

pub fn offline_uuid_bytes(name: &str) -> [u8; 16] {
    let mut h = Md5::new();
    h.update(format!("OfflinePlayer:{name}").as_bytes());
    let mut b: [u8; 16] = h.finalize().into();
    b[6] = (b[6] & 0x0f) | 0x30; // versão 3
    b[8] = (b[8] & 0x3f) | 0x80; // variante IETF
    b
}

/// Formato com hifens (8-4-4-4-12).
pub fn offline_uuid(name: &str) -> String {
    let h = hex::encode(offline_uuid_bytes(name));
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

/// Formato sem hifens (como o launcher oficial passa em --uuid).
pub fn offline_uuid_undashed(name: &str) -> String {
    hex::encode(offline_uuid_bytes(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_conhecido_do_notch() {
        // Valor de referência conferido contra java.util.UUID.nameUUIDFromBytes (ver README/etapa de testes).
        assert_eq!(offline_uuid("Notch"), "b50ad385-829d-3141-a216-7e7d7539ba7f");
        assert_eq!(offline_uuid("Bruno_123"), "a832fe17-2c37-3634-bd3e-8476896e9344");
    }

    #[test]
    fn versao_e_variante() {
        let u = offline_uuid("QualquerNome");
        assert_eq!(u.as_bytes()[14], b'3'); // versão 3
        assert!("89ab".contains(u.as_bytes()[19] as char)); // variante
        assert_eq!(u.replace('-', ""), offline_uuid_undashed("QualquerNome"));
    }

    #[test]
    fn diferencia_maiusculas() {
        assert_ne!(offline_uuid("notch"), offline_uuid("Notch"));
    }
}
