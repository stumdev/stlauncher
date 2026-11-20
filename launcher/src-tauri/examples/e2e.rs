//! Teste ponta a ponta SEM interface: roda o pipeline real (API local + Mojang + Adoptium + NeoForge)
//! e abre o jogo. Uso:
//!   cargo run --example e2e -- <API_URL> <PASTA_RAIZ> <NICK>
//! Espera o jogo chegar ao ponto de carregar recursos/conectar e então o encerra.
use stlauncher_lib::config::AppConfig;
use stlauncher_lib::download;
use stlauncher_lib::pipeline::{self, Ctx};
use stlauncher_lib::report::{Report, Reporter};
use stlauncher_lib::settings::Settings;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[tokio::main]
async fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (api, root, nick) = (a[1].clone(), a[2].clone(), a[3].clone());
    let full = a.get(4).map(|s| s == "full").unwrap_or(false);

    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let l2 = lines.clone();
    let last_stage = Arc::new(Mutex::new(String::new()));
    let ls = last_stage.clone();
    let rep: Reporter = Arc::new(move |r| match r {
        Report::Stage { label } => {
            println!("[etapa] {label}");
            *ls.lock().unwrap() = label;
        }
        Report::Progress { label, done_bytes, total_bytes, files_done, files_total, speed_bps, .. } => {
            // imprime só de vez em quando para não poluir
            if files_done == files_total || done_bytes % 7 == 0 {
                let _ = (label, done_bytes, total_bytes, speed_bps);
            }
        }
        Report::Warning { message } => println!("[aviso] {message}"),
        Report::Log { line, stream } => {
            if stream == "installer" {
                println!("[installer] {line}");
            }
            l2.lock().unwrap().push(line);
        }
        Report::GameStarted => println!("[jogo] iniciado"),
        Report::GameExit { code, crashed, summary, .. } => println!("[jogo] saiu code={code:?} crashed={crashed} resumo={summary:?}"),
    });

    let settings = Settings { nickname: nick, game_dir: root, ..Settings::default() };
    let ctx = Ctx {
        cfg: AppConfig { name: "E2E".into(), api_url: api },
        settings,
        client: download::build_client().unwrap(),
        rep,
        app_version: "0.0.0-e2e".into(),
    };

    let t0 = std::time::Instant::now();
    // "prepare" = instala/atualiza tudo e monta o comando, mas NÃO abre o jogo (sem janela).
    if a.get(4).map(|s| s == "prepare").unwrap_or(false) {
        match pipeline::prepare(&ctx, false).await {
            Ok(p) => {
                println!("[ok] preparado em {:?}; offline={}", t0.elapsed(), p.offline);
                let neo = p.cmd.iter().find(|x| x.contains("neoForgeVersion") || x.starts_with("21.1.")).cloned();
                println!("[cmd] {} argumentos; neoforge={:?}", p.cmd.len(), neo);
                std::process::exit(0);
            }
            Err(e) => {
                println!("ERRO: {e}");
                std::process::exit(2);
            }
        }
    }
    let (mut handle, prepared) = match pipeline::play(&ctx, full).await {
        Ok(x) => x,
        Err(e) => {
            println!("ERRO: {e}");
            std::process::exit(2);
        }
    };
    println!("[ok] preparação em {:?}; offline={}", t0.elapsed(), prepared.offline);
    println!("[cmd] {}", prepared.cmd.iter().take(3).cloned().collect::<Vec<_>>().join(" "));

    // Aguarda sinais de que o jogo carregou (ou crash).
    let deadline = std::time::Instant::now() + Duration::from_secs(150);
    let mut reached = false;
    while std::time::Instant::now() < deadline && !handle.task.is_finished() {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let l = lines.lock().unwrap();
        if l.iter().any(|x| x.contains("Sound engine started") || x.contains("Connecting to") || x.contains("Created: 1024x")) {
            reached = true;
            break;
        }
    }
    if handle.task.is_finished() {
        let info = handle.task.await.unwrap();
        println!("[jogo] terminou sozinho: {info:?}");
    } else {
        handle.kill();
        let _ = handle.task.await;
    }
    let l = lines.lock().unwrap();
    println!("--- últimas 25 linhas do log ({} total) ---", l.len());
    for x in l.iter().rev().take(25).rev() {
        println!("{x}");
    }
    for key in ["Setting user", "Connecting to", "Sound engine started", "Created: 1024x"] {
        println!("[sinal] {key}: {}", l.iter().find(|x| x.contains(key)).cloned().unwrap_or_else(|| "NÃO ENCONTRADO".into()));
    }
    println!("RESULTADO: {}", if reached { "JOGO CHEGOU AO MENU/CONEXÃO" } else { "NÃO CONFIRMADO" });
    std::process::exit(if reached { 0 } else { 3 });
}
