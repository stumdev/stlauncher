//! Status do servidor via "Server List Ping" do protocolo do Minecraft (TCP, sem depender da API).
use crate::error::{AppError, Result};
use serde::Serialize;
use serde_json::Value;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PACKET: usize = 4 * 1024 * 1024; // o favicon em base64 pode ser grande

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatus {
    pub online: bool,
    pub players_online: u32,
    pub players_max: u32,
    pub motd: String,
    pub version: String,
    pub latency_ms: u64,
}

/// "host" ou "host:porta" -> (host, porta). Porta padrão 25565.
pub fn parse_address(addr: &str) -> (String, u16) {
    let addr = addr.trim();
    if let Some((h, p)) = addr.rsplit_once(':') {
        if let Ok(port) = p.parse::<u16>() {
            if !h.is_empty() && !h.contains(':') {
                return (h.to_string(), port);
            }
        }
    }
    (addr.to_string(), 25565)
}

pub fn write_varint(buf: &mut Vec<u8>, v: i32) {
    let mut v = v as u32;
    loop {
        if v & !0x7f == 0 {
            buf.push(v as u8);
            return;
        }
        buf.push((v & 0x7f) as u8 | 0x80);
        v >>= 7;
    }
}

/// Lê um VarInt de um slice, avançando-o.
pub fn read_varint_slice(s: &mut &[u8]) -> Result<i32> {
    let mut val: u32 = 0;
    for i in 0..5 {
        let b = *s.first().ok_or_else(|| AppError::new("resposta truncada"))?;
        *s = &s[1..];
        val |= u32::from(b & 0x7f) << (7 * i);
        if b & 0x80 == 0 {
            return Ok(val as i32);
        }
    }
    Err(AppError::new("VarInt grande demais"))
}

async fn read_varint(st: &mut TcpStream) -> Result<i32> {
    let mut val: u32 = 0;
    for i in 0..5 {
        let b = st.read_u8().await?;
        val |= u32::from(b & 0x7f) << (7 * i);
        if b & 0x80 == 0 {
            return Ok(val as i32);
        }
    }
    Err(AppError::new("VarInt grande demais"))
}

/// Achata o campo "description" (string ou objeto de chat) e remove códigos de cor §x.
pub fn flatten_motd(v: &Value) -> String {
    fn walk(v: &Value, out: &mut String) {
        match v {
            Value::String(s) => out.push_str(s),
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            Value::Object(o) => {
                if let Some(Value::String(t)) = o.get("text") {
                    out.push_str(t);
                }
                if let Some(e) = o.get("extra") {
                    walk(e, out);
                }
            }
            _ => {}
        }
    }
    let mut raw = String::new();
    walk(v, &mut raw);
    let mut out = String::new();
    let mut it = raw.chars();
    while let Some(c) = it.next() {
        if c == '§' {
            it.next();
        } else {
            out.push(c);
        }
    }
    out.trim().to_string()
}

pub fn parse_status_json(json: &str) -> Result<ServerStatus> {
    let v: Value = serde_json::from_str(json)?;
    Ok(ServerStatus {
        online: true,
        players_online: v["players"]["online"].as_u64().unwrap_or(0) as u32,
        players_max: v["players"]["max"].as_u64().unwrap_or(0) as u32,
        motd: flatten_motd(&v["description"]),
        version: v["version"]["name"].as_str().unwrap_or("").to_string(),
        latency_ms: 0,
    })
}

async fn query(host: &str, port: u16) -> Result<ServerStatus> {
    let mut st = TcpStream::connect((host, port)).await?;
    let _ = st.set_nodelay(true);

    // Handshake (estado 1 = status) + pedido de status.
    let mut hs = Vec::new();
    write_varint(&mut hs, 0x00);
    write_varint(&mut hs, 767); // protocolo do 1.21.1; o servidor responde status para qualquer valor
    write_varint(&mut hs, host.len() as i32);
    hs.extend_from_slice(host.as_bytes());
    hs.extend_from_slice(&port.to_be_bytes());
    write_varint(&mut hs, 1);
    let mut out = Vec::new();
    write_varint(&mut out, hs.len() as i32);
    out.extend_from_slice(&hs);
    out.extend_from_slice(&[0x01, 0x00]);
    let t0 = Instant::now();
    st.write_all(&out).await?;

    let len = read_varint(&mut st).await?;
    if len <= 0 || len as usize > MAX_PACKET {
        return Err(AppError::new("resposta de status inválida"));
    }
    let mut buf = vec![0u8; len as usize];
    st.read_exact(&mut buf).await?;
    let latency = t0.elapsed().as_millis() as u64;

    let mut s: &[u8] = &buf;
    let id = read_varint_slice(&mut s)?;
    if id != 0 {
        return Err(AppError::new("pacote de status inesperado"));
    }
    let jl = read_varint_slice(&mut s)? as usize;
    if jl > s.len() {
        return Err(AppError::new("JSON de status truncado"));
    }
    let mut status = parse_status_json(&String::from_utf8_lossy(&s[..jl]))?;
    status.latency_ms = latency;
    Ok(status)
}

/// Consulta o servidor. Nunca falha: qualquer erro/timeout vira `online: false`.
pub async fn ping(address: &str) -> ServerStatus {
    let (host, port) = parse_address(address);
    match tokio::time::timeout(TIMEOUT, query(&host, port)).await {
        Ok(Ok(s)) => s,
        _ => ServerStatus::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn varint_roundtrip() {
        for v in [0, 1, 127, 128, 255, 25565, 767, 2097151, i32::MAX, -1] {
            let mut b = Vec::new();
            write_varint(&mut b, v);
            let mut s: &[u8] = &b;
            assert_eq!(read_varint_slice(&mut s).unwrap(), v);
            assert!(s.is_empty());
        }
        // 300 = 0xAC 0x02 (exemplo da wiki.vg)
        let mut b = Vec::new();
        write_varint(&mut b, 300);
        assert_eq!(b, vec![0xAC, 0x02]);
    }

    #[test]
    fn endereco() {
        assert_eq!(parse_address("play.x.com:25570"), ("play.x.com".into(), 25570));
        assert_eq!(parse_address("play.x.com"), ("play.x.com".into(), 25565));
        assert_eq!(parse_address(" 1.2.3.4:1 "), ("1.2.3.4".into(), 1));
        assert_eq!(parse_address("x:abc"), ("x:abc".into(), 25565));
    }

    #[test]
    fn motd_achata_e_remove_cores() {
        let v: Value = serde_json::from_str(r#"{"text":"§aBem-vindo ","extra":[{"text":"ao §lserver","color":"gold"},"!"]}"#).unwrap();
        assert_eq!(flatten_motd(&v), "Bem-vindo ao server!");
        assert_eq!(flatten_motd(&Value::String("§6Oi".into())), "Oi");
    }

    #[tokio::test]
    async fn ping_contra_servidor_falso() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut b = [0u8; 256];
            let n = s.read(&mut b).await.unwrap();
            // handshake: len, id=0, proto(767 = 2 bytes), host, porta, next=1 ; depois 01 00
            assert!(n >= 5);
            assert_eq!(&b[n - 2..n], &[0x01, 0x00]);
            let json = r#"{"version":{"name":"1.21.1","protocol":767},"players":{"max":20,"online":3},"description":{"text":"§aOlá"}}"#;
            let mut body = Vec::new();
            write_varint(&mut body, 0);
            write_varint(&mut body, json.len() as i32);
            body.extend_from_slice(json.as_bytes());
            let mut pkt = Vec::new();
            write_varint(&mut pkt, body.len() as i32);
            pkt.extend_from_slice(&body);
            s.write_all(&pkt).await.unwrap();
        });
        let st = ping(&format!("127.0.0.1:{port}")).await;
        assert!(st.online);
        assert_eq!((st.players_online, st.players_max), (3, 20));
        assert_eq!(st.motd, "Olá");
        assert_eq!(st.version, "1.21.1");
    }

    #[tokio::test]
    async fn servidor_fora_do_ar_vira_offline() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        assert!(!ping(&format!("127.0.0.1:{port}")).await.online);
    }
}
