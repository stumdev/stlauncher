//! version.json do Minecraft/NeoForge: tipos, regras (rules), merge (inheritsFrom) e montagem do comando de launch.
use crate::error::{AppError, Result};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct VersionJson {
    pub id: String,
    pub inherits_from: Option<String>,
    pub main_class: Option<String>,
    pub arguments: Option<Arguments>,
    #[serde(default)]
    pub libraries: Vec<Library>,
    pub asset_index: Option<AssetIndexRef>,
    pub assets: Option<String>,
    pub downloads: Option<Downloads>,
    #[serde(rename = "type")]
    pub version_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssetIndexRef {
    pub id: String,
    pub sha1: String,
    pub size: u64,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Downloads {
    pub client: Option<Artifact>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Artifact {
    pub path: Option<String>,
    pub sha1: Option<String>,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Arguments {
    #[serde(default)]
    pub game: Vec<Arg>,
    #[serde(default)]
    pub jvm: Vec<Arg>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Arg {
    Plain(String),
    Ruled { rules: Vec<Rule>, value: ArgValue },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ArgValue {
    One(String),
    Many(Vec<String>),
}

#[derive(Debug, Clone, Deserialize)]
pub struct Rule {
    pub action: String,
    pub os: Option<OsRule>,
    pub features: Option<HashMap<String, bool>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OsRule {
    pub name: Option<String>,
    pub arch: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Library {
    pub name: String,
    pub downloads: Option<LibDownloads>,
    pub rules: Option<Vec<Rule>>,
    /// Base Maven (formato antigo, sem `downloads`).
    pub url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LibDownloads {
    pub artifact: Option<Artifact>,
}

// ---------- regras ----------

fn os_name() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "osx"
    } else {
        "linux"
    }
}

pub type Features = HashMap<String, bool>;

fn rule_matches(r: &Rule, features: &Features) -> bool {
    if let Some(os) = &r.os {
        if let Some(n) = &os.name {
            if n != os_name() {
                return false;
            }
        }
        // "x86" = 32 bits; só suportamos x64.
        if os.arch.as_deref() == Some("x86") {
            return false;
        }
    }
    if let Some(f) = &r.features {
        for (k, v) in f {
            if features.get(k).copied().unwrap_or(false) != *v {
                return false;
            }
        }
    }
    true
}

/// Semântica da Mojang: sem regras => permitido; senão a ÚLTIMA regra que casa decide (padrão: negado).
pub fn rules_allow(rules: &[Rule], features: &Features) -> bool {
    if rules.is_empty() {
        return true;
    }
    let mut allowed = false;
    for r in rules {
        if rule_matches(r, features) {
            allowed = r.action == "allow";
        }
    }
    allowed
}

// ---------- bibliotecas ----------

/// "grupo:artefato:versão[:classificador][@ext]" -> caminho Maven relativo.
pub fn maven_path(name: &str) -> Option<String> {
    let (coord, ext) = match name.split_once('@') {
        Some((c, e)) => (c, e),
        None => (name, "jar"),
    };
    let p: Vec<&str> = coord.split(':').collect();
    if p.len() < 3 {
        return None;
    }
    let (g, a, v) = (p[0].replace('.', "/"), p[1], p[2]);
    let cls = p.get(3).map(|c| format!("-{c}")).unwrap_or_default();
    Some(format!("{g}/{a}/{v}/{a}-{v}{cls}.{ext}"))
}

/// Chave para deduplicar: grupo:artefato[:classificador] (sem versão).
fn lib_key(name: &str) -> String {
    let coord = name.split('@').next().unwrap_or(name);
    let p: Vec<&str> = coord.split(':').collect();
    match p.len() {
        0..=2 => coord.to_string(),
        3 => format!("{}:{}", p[0], p[1]),
        _ => format!("{}:{}:{}", p[0], p[1], p[3]),
    }
}

impl Library {
    /// Artefato resolvido (path relativo garantido) ou None se a lib não tem arquivo.
    pub fn artifact(&self) -> Option<Artifact> {
        if let Some(a) = self.downloads.as_ref().and_then(|d| d.artifact.clone()) {
            let path = a.path.clone().or_else(|| maven_path(&self.name))?;
            return Some(Artifact { path: Some(path), ..a });
        }
        let path = maven_path(&self.name)?;
        let base = self.url.as_ref()?;
        Some(Artifact { url: format!("{}/{}", base.trim_end_matches('/'), path), path: Some(path), sha1: None, size: 0 })
    }
    pub fn applies(&self, features: &Features) -> bool {
        self.rules.as_ref().map_or(true, |r| rules_allow(r, features))
    }
}

/// Bibliotecas aplicáveis a esta máquina que possuem arquivo.
pub fn active_libraries<'a>(v: &'a VersionJson, features: &Features) -> Vec<(&'a Library, Artifact)> {
    v.libraries.iter().filter(|l| l.applies(features)).filter_map(|l| l.artifact().map(|a| (l, a))).collect()
}

// ---------- merge ----------

/// Mescla o perfil filho (NeoForge) sobre o pai (vanilla): libs deduplicadas (filho vence),
/// argumentos concatenados (pai primeiro), mainClass do filho, assets/downloads do pai.
pub fn merge(parent: &VersionJson, child: &VersionJson) -> VersionJson {
    let mut seen = HashSet::new();
    let mut libraries = Vec::new();
    for l in child.libraries.iter().chain(parent.libraries.iter()) {
        if seen.insert(lib_key(&l.name)) {
            libraries.push(l.clone());
        }
    }
    let join = |a: Option<&Arguments>, b: Option<&Arguments>, game: bool| -> Vec<Arg> {
        let pick = |x: Option<&Arguments>| x.map(|x| if game { x.game.clone() } else { x.jvm.clone() }).unwrap_or_default();
        let mut v = pick(a);
        v.extend(pick(b));
        v
    };
    let arguments = Arguments {
        game: join(parent.arguments.as_ref(), child.arguments.as_ref(), true),
        jvm: join(parent.arguments.as_ref(), child.arguments.as_ref(), false),
    };
    VersionJson {
        id: child.id.clone(),
        inherits_from: child.inherits_from.clone(),
        main_class: child.main_class.clone().or_else(|| parent.main_class.clone()),
        arguments: Some(arguments),
        libraries,
        asset_index: child.asset_index.clone().or_else(|| parent.asset_index.clone()),
        assets: child.assets.clone().or_else(|| parent.assets.clone()),
        downloads: child.downloads.clone().or_else(|| parent.downloads.clone()),
        version_type: child.version_type.clone().or_else(|| parent.version_type.clone()),
    }
}

// ---------- comando de launch ----------

#[derive(Debug, Clone)]
pub struct LaunchParams {
    pub java: PathBuf,
    pub root: PathBuf,
    pub game_dir: PathBuf,
    pub username: String,
    /// UUID sem hifens.
    pub uuid: String,
    pub xmx_mb: u32,
    pub xms_mb: u32,
    pub extra_jvm: Vec<String>,
    pub resolution: Option<(u32, u32)>,
    /// Endereço "host:porta" para conectar direto ao iniciar.
    pub quick_play: Option<String>,
    pub launcher_name: String,
    pub launcher_version: String,
    /// ${version_name} nos argumentos do JOGO (id do perfil lançado, ex: neoforge-21.1.172).
    pub version_name: String,
    /// ${version_name} nos argumentos da JVM: id do jar vanilla (ex: 1.21.1). O NeoForge usa
    /// "-DignoreList=...,${version_name}.jar" para ignorar o jar do Minecraft no module path;
    /// com o id errado o jogo cai em "Modules minecraft and _1._21._1 export package ...".
    pub jar_version: String,
}

/// Separa uma string de argumentos respeitando aspas simples/duplas.
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    for c in s.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"') | (None, '\'') => {
                quote = Some(c);
                has = true;
            }
            (None, c) if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub fn features_for(p: &LaunchParams) -> Features {
    let mut f = Features::new();
    f.insert("has_custom_resolution".into(), p.resolution.is_some());
    f.insert("is_quick_play_multiplayer".into(), p.quick_play.is_some());
    for k in ["is_demo_user", "is_quick_play_singleplayer", "is_quick_play_realms"] {
        f.insert(k.into(), false);
    }
    f
}

fn expand_args(args: &[Arg], features: &Features) -> Vec<String> {
    let mut out = Vec::new();
    for a in args {
        match a {
            Arg::Plain(s) => out.push(s.clone()),
            Arg::Ruled { rules, value } if rules_allow(rules, features) => match value {
                ArgValue::One(s) => out.push(s.clone()),
                ArgValue::Many(v) => out.extend(v.iter().cloned()),
            },
            _ => {}
        }
    }
    out
}

/// Substitui ${chave}. Devolve as chaves desconhecidas para sinalizar erro.
fn substitute(s: &str, vars: &HashMap<&str, String>, missing: &mut Vec<String>) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("${") {
        out.push_str(&rest[..i]);
        match rest[i + 2..].find('}') {
            Some(j) => {
                let key = &rest[i + 2..i + 2 + j];
                match vars.get(key) {
                    Some(v) => out.push_str(v),
                    None => {
                        missing.push(key.to_string());
                        out.push_str(&rest[i..i + 3 + j]);
                    }
                }
                rest = &rest[i + 3 + j..];
            }
            None => {
                out.push_str(&rest[i..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

pub const CP_SEP: &str = if cfg!(windows) { ";" } else { ":" };

/// Classpath: libs aplicáveis (na ordem do perfil) + jar do cliente. Remove caminhos repetidos.
pub fn classpath(v: &VersionJson, features: &Features, libraries_dir: &Path, client_jar: &Path) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut cp = Vec::new();
    for (_, a) in active_libraries(v, features) {
        let p = libraries_dir.join(a.path.as_deref().unwrap_or_default());
        if seen.insert(p.clone()) {
            cp.push(p);
        }
    }
    cp.push(client_jar.to_path_buf());
    cp
}

/// Monta a linha de comando completa: [java, jvm..., mainClass, game...].
pub fn build_command(v: &VersionJson, p: &LaunchParams, cp: &[PathBuf], natives_dir: &Path, assets_dir: &Path) -> Result<Vec<String>> {
    let features = features_for(p);
    let main = v.main_class.clone().ok_or_else(|| AppError::new("Perfil do jogo sem mainClass (instalação corrompida). Use \"Reparar instalação\"."))?;
    let args = v.arguments.as_ref().ok_or_else(|| AppError::new("Perfil do jogo sem argumentos (instalação corrompida). Use \"Reparar instalação\"."))?;

    let s = |p: &Path| p.to_string_lossy().into_owned();
    let (w, h) = p.resolution.unwrap_or((0, 0));
    let mut vars: HashMap<&str, String> = HashMap::new();
    vars.insert("auth_player_name", p.username.clone());
    vars.insert("version_name", p.version_name.clone());
    vars.insert("game_directory", s(&p.game_dir));
    vars.insert("assets_root", s(assets_dir));
    vars.insert("assets_index_name", v.assets.clone().unwrap_or_default());
    vars.insert("auth_uuid", p.uuid.clone());
    vars.insert("auth_access_token", "0".into());
    vars.insert("clientid", "0".into());
    vars.insert("auth_xuid", "0".into());
    vars.insert("user_type", "legacy".into());
    vars.insert("version_type", v.version_type.clone().unwrap_or_else(|| "release".into()));
    vars.insert("resolution_width", w.to_string());
    vars.insert("resolution_height", h.to_string());
    vars.insert("quickPlayMultiplayer", p.quick_play.clone().unwrap_or_default());
    vars.insert("quickPlayPath", String::new());
    vars.insert("quickPlaySingleplayer", String::new());
    vars.insert("quickPlayRealms", String::new());
    vars.insert("launcher_name", p.launcher_name.clone());
    vars.insert("launcher_version", p.launcher_version.clone());
    vars.insert("natives_directory", s(natives_dir));
    vars.insert("library_directory", s(&p.root.join("libraries")));
    vars.insert("classpath_separator", CP_SEP.into());
    vars.insert("classpath", cp.iter().map(|x| s(x)).collect::<Vec<_>>().join(CP_SEP));

    let mut missing = Vec::new();
    let mut jvm_vars = vars.clone();
    jvm_vars.insert("version_name", p.jar_version.clone());
    let jvm: Vec<String> = expand_args(&args.jvm, &features).iter().map(|a| substitute(a, &jvm_vars, &mut missing)).collect();
    let game: Vec<String> = expand_args(&args.game, &features).iter().map(|a| substitute(a, &vars, &mut missing)).collect();
    if !missing.is_empty() {
        missing.sort();
        missing.dedup();
        return Err(AppError::new(format!("Perfil do jogo usa variáveis desconhecidas ({}). Atualize o launcher.", missing.join(", "))));
    }

    let mut cmd = vec![s(&p.java), format!("-Xms{}M", p.xms_mb), format!("-Xmx{}M", p.xmx_mb)];
    cmd.extend(p.extra_jvm.iter().cloned());
    cmd.extend(jvm);
    // Perfis antigos podem não trazer -cp nos argumentos JVM.
    if !cmd.iter().any(|a| a == "-cp" || a == "-classpath") {
        cmd.push("-cp".into());
        cmd.push(vars["classpath"].clone());
    }
    cmd.push(main);
    cmd.extend(game);
    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Versões reduzidas dos JSON reais (vanilla 1.21.1 e NeoForge 21.1.x).
    const PARENT: &str = r#"{
      "id":"1.21.1","type":"release","mainClass":"net.minecraft.client.main.Main","assets":"17",
      "assetIndex":{"id":"17","sha1":"aa","size":1,"url":"http://x/17.json"},
      "downloads":{"client":{"sha1":"bb","size":5,"url":"http://x/client.jar"}},
      "arguments":{
        "game":["--username","${auth_player_name}","--version","${version_name}","--gameDir","${game_directory}",
          "--assetsDir","${assets_root}","--assetIndex","${assets_index_name}","--uuid","${auth_uuid}",
          "--accessToken","${auth_access_token}","--clientId","${clientid}","--xuid","${auth_xuid}",
          "--userType","${user_type}","--versionType","${version_type}",
          {"rules":[{"action":"allow","features":{"is_demo_user":true}}],"value":"--demo"},
          {"rules":[{"action":"allow","features":{"has_custom_resolution":true}}],"value":["--width","${resolution_width}","--height","${resolution_height}"]},
          {"rules":[{"action":"allow","features":{"is_quick_play_multiplayer":true}}],"value":["--quickPlayMultiplayer","${quickPlayMultiplayer}"]},
          {"rules":[{"action":"allow","features":{"is_quick_play_singleplayer":true}}],"value":["--quickPlaySingleplayer","${quickPlaySingleplayer}"]}],
        "jvm":[
          {"rules":[{"action":"allow","os":{"name":"osx"}}],"value":["-XstartOnFirstThread"]},
          {"rules":[{"action":"allow","os":{"name":"windows"}}],"value":"-XX:HeapDumpPath=MojangTricksIntelDriversForPerformance_javaw.exe_minecraft.exe.heapdump"},
          {"rules":[{"action":"allow","os":{"arch":"x86"}}],"value":"-Xss1M"},
          "-Djava.library.path=${natives_directory}","-Dminecraft.launcher.brand=${launcher_name}",
          "-Dminecraft.launcher.version=${launcher_version}","-cp","${classpath}"]},
      "libraries":[
        {"name":"com.mojang:logging:1.2.7","downloads":{"artifact":{"path":"com/mojang/logging/1.2.7/logging-1.2.7.jar","sha1":"c1","size":1,"url":"http://x/l.jar"}}},
        {"name":"org.ow2.asm:asm:9.3","downloads":{"artifact":{"path":"org/ow2/asm/asm/9.3/asm-9.3.jar","sha1":"c2","size":1,"url":"http://x/asm93.jar"}}},
        {"name":"org.lwjgl:lwjgl:3.3.3:natives-windows","rules":[{"action":"allow","os":{"name":"windows"}}],"downloads":{"artifact":{"path":"org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3-natives-windows.jar","sha1":"c3","size":1,"url":"http://x/n.jar"}}},
        {"name":"org.lwjgl:lwjgl:3.3.3:natives-linux","rules":[{"action":"allow","os":{"name":"linux"}}],"downloads":{"artifact":{"path":"org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3-natives-linux.jar","sha1":"c4","size":1,"url":"http://x/nl.jar"}}}
      ]}"#;
    const CHILD: &str = r#"{
      "id":"neoforge-21.1.172","inheritsFrom":"1.21.1","type":"release","mainClass":"cpw.mods.bootstraplauncher.BootstrapLauncher",
      "arguments":{
        "game":["--launchTarget","forgeclient","--fml.neoForgeVersion","21.1.172"],
        "jvm":["-DignoreList=a,b,${version_name}.jar","-DlibraryDirectory=${library_directory}","-p",
               "${library_directory}/cpw/mods/bootstraplauncher/2.0.2/bootstraplauncher-2.0.2.jar${classpath_separator}${library_directory}/x.jar",
               "--add-modules","ALL-MODULE-PATH"]},
      "libraries":[
        {"name":"org.ow2.asm:asm:9.7","downloads":{"artifact":{"path":"org/ow2/asm/asm/9.7/asm-9.7.jar","sha1":"d1","size":1,"url":"http://x/asm97.jar"}}},
        {"name":"net.neoforged:neoforge:21.1.172:universal","downloads":{"artifact":{"path":"net/neoforged/neoforge/21.1.172/neoforge-21.1.172-universal.jar","sha1":"d2","size":1,"url":""}}}
      ]}"#;

    fn params() -> LaunchParams {
        LaunchParams {
            java: PathBuf::from("C:/jre/bin/java.exe"),
            root: PathBuf::from("C:/Users/João Silva/AppData/Local/ST/data"),
            game_dir: PathBuf::from("C:/Users/João Silva/AppData/Local/ST/data/game"),
            username: "Bruno_1".into(),
            uuid: "a832fe172c373634bd3e8476896e9344".into(),
            xmx_mb: 6144,
            xms_mb: 1024,
            extra_jvm: vec!["-XX:+UseG1GC".into()],
            resolution: None,
            quick_play: Some("play.exemplo.com:25565".into()),
            launcher_name: "STLauncher".into(),
            launcher_version: "1.0.0".into(),
            version_name: "neoforge-21.1.172".into(),
            jar_version: "1.21.1".into(),
        }
    }

    fn merged() -> VersionJson {
        let p: VersionJson = serde_json::from_str(PARENT).unwrap();
        let c: VersionJson = serde_json::from_str(CHILD).unwrap();
        merge(&p, &c)
    }

    #[test]
    fn merge_dedupe_e_campos() {
        let m = merged();
        assert_eq!(m.id, "neoforge-21.1.172");
        assert_eq!(m.main_class.as_deref(), Some("cpw.mods.bootstraplauncher.BootstrapLauncher"));
        assert_eq!(m.assets.as_deref(), Some("17"));
        assert!(m.downloads.unwrap().client.is_some());
        let asm: Vec<_> = m.libraries.iter().filter(|l| l.name.contains("org.ow2.asm:asm:")).collect();
        assert_eq!(asm.len(), 1);
        assert_eq!(asm[0].name, "org.ow2.asm:asm:9.7"); // filho vence
    }

    #[test]
    fn rules_de_os_filtram_libs() {
        let m = merged();
        let libs = active_libraries(&m, &Features::new());
        let names: Vec<_> = libs.iter().map(|(l, _)| l.name.as_str()).collect();
        if cfg!(windows) {
            assert!(names.iter().any(|n| n.ends_with("natives-windows")));
            assert!(!names.iter().any(|n| n.ends_with("natives-linux")));
        }
    }

    #[test]
    fn comando_completo() {
        let m = merged();
        let p = params();
        let feats = features_for(&p);
        let cp = classpath(&m, &feats, &p.root.join("libraries"), &p.root.join("versions/1.21.1/1.21.1.jar"));
        let cmd = build_command(&m, &p, &cp, &p.root.join("versions/neoforge-21.1.172/natives"), &p.root.join("assets")).unwrap();
        let j = cmd.join("\u{1}");
        let has = |a: &str| cmd.iter().any(|x| x == a);
        assert_eq!(cmd[0], "C:/jre/bin/java.exe");
        assert!(has("-Xmx6144M") && has("-Xms1024M") && has("-XX:+UseG1GC"));
        assert!(has("-DignoreList=a,b,1.21.1.jar")); // nome do jar vanilla, não o id do perfil
        assert!(has("--add-modules"));
        // placeholders resolvidos, inclusive com acento e espaço no caminho
        assert!(!j.contains("${"));
        assert!(cmd.iter().any(|a| a.starts_with("-DlibraryDirectory=C:/Users/João Silva")));
        let mp = &cmd[cmd.iter().position(|a| a == "-p").unwrap() + 1];
        assert!(mp.contains(CP_SEP) && !mp.contains("${"));
        // classpath: contém lib de asm 9.7 e NÃO a 9.3; termina com o jar do cliente
        let cpi = cmd.iter().position(|a| a == "-cp").unwrap() + 1;
        assert!(cmd[cpi].contains("asm-9.7.jar") && !cmd[cpi].contains("asm-9.3.jar"));
        assert!(cmd[cpi].ends_with("1.21.1.jar"));
        // ordem: jvm... mainClass game...
        let mi = cmd.iter().position(|a| a == "cpw.mods.bootstraplauncher.BootstrapLauncher").unwrap();
        assert!(mi > cpi);
        let game = &cmd[mi + 1..];
        let pair = |k: &str, v: &str| game.windows(2).any(|w| w[0] == k && w[1] == v);
        assert!(pair("--username", "Bruno_1"));
        assert!(pair("--uuid", "a832fe172c373634bd3e8476896e9344"));
        assert!(pair("--accessToken", "0"));
        assert!(pair("--userType", "legacy"));
        assert!(pair("--version", "neoforge-21.1.172"));
        assert!(pair("--assetIndex", "17"));
        assert!(pair("--quickPlayMultiplayer", "play.exemplo.com:25565"));
        assert!(pair("--launchTarget", "forgeclient"));
        assert!(!game.contains(&"--demo".to_string()));
        assert!(!game.contains(&"--width".to_string()));
        assert!(!cmd.contains(&"-Xss1M".to_string()));
    }

    #[test]
    fn quickplay_off_e_resolucao_on() {
        let m = merged();
        let mut p = params();
        p.quick_play = None;
        p.resolution = Some((1280, 720));
        let cp = classpath(&m, &features_for(&p), &p.root.join("libraries"), Path::new("c.jar"));
        let cmd = build_command(&m, &p, &cp, Path::new("n"), Path::new("a")).unwrap();
        assert!(!cmd.contains(&"--quickPlayMultiplayer".to_string()));
        assert!(cmd.windows(4).any(|w| w == ["--width", "1280", "--height", "720"]));
    }

    #[test]
    fn variavel_desconhecida_vira_erro() {
        let mut m = merged();
        m.arguments.as_mut().unwrap().game.push(Arg::Plain("${nao_existe}".into()));
        let p = params();
        let e = build_command(&m, &p, &[], Path::new("n"), Path::new("a")).unwrap_err();
        assert!(e.0.contains("nao_existe"));
    }

    #[test]
    fn maven_paths() {
        assert_eq!(maven_path("net.neoforged:neoforge:21.1.172:universal").unwrap(), "net/neoforged/neoforge/21.1.172/neoforge-21.1.172-universal.jar");
        assert_eq!(maven_path("a.b:c:1.0@zip").unwrap(), "a/b/c/1.0/c-1.0.zip");
        assert!(maven_path("invalido").is_none());
    }

    #[test]
    fn split_args_com_aspas() {
        assert_eq!(split_args("-Xa -Db=\"c d\"  'e f' g"), vec!["-Xa", "-Db=c d", "e f", "g"]);
        assert!(split_args("   ").is_empty());
    }
}
