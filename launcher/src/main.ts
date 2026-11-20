import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import "./style.css";
import { escapeHtml, formatBytes, formatEta, renderMarkdown } from "./markdown.ts";
import { drawTerrain } from "./terrain.ts";

// ---------- tipos (espelham o backend em Rust) ----------
interface Settings {
  nickname: string;
  ramMb: number;
  gameDir: string;
  jvmArgs: string;
  width: number;
  height: number;
  quickPlay: boolean;
}
interface InitInfo {
  name: string;
  version: string;
  settings: Settings;
  totalRamMb: number;
  defaultRamMb: number;
  defaultJvmArgs: string;
  gameRoot: string;
}
interface PackInfo {
  packVersion: string;
  news: string;
  serverAddress: string;
  minRamMb: number;
  offline: boolean;
}
interface ServerStatus {
  online: boolean;
  playersOnline: number;
  playersMax: number;
  motd: string;
  version: string;
  latencyMs: number;
}
type ProgressEvent = { type: "progress"; label: string; doneBytes: number; totalBytes: number; filesDone: number; filesTotal: number; speedBps: number; etaSecs: number | null };
type LauncherEvent =
  | { type: "stage"; label: string }
  | ProgressEvent
  | { type: "warning"; message: string }
  | { type: "log"; line: string; stream: string }
  | { type: "gameStarted" }
  | { type: "gameExit"; code: number | null; crashed: boolean; summary: string | null; crashReport: string | null };

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const NICK_RE = /^[A-Za-z0-9_]{3,16}$/;
const BLOCKS = 20;
const UPDATE_LABEL = "Atualizando o launcher";

let init: InitInfo;
let settings: Settings;
let pack: PackInfo | null = null;
let busy = false; // instalando/reparando/jogando
let playing = false;
const logLines: string[] = [];
const MAX_LOG = 5000;
let logDirty = false;

// ---------- utilidades ----------
const show = (el: HTMLElement, on = true) => el.classList.toggle("hidden", !on);
const errText = (e: unknown) => (typeof e === "string" ? e : e instanceof Error ? e.message : JSON.stringify(e));
const gb = (mb: number) => `${(mb / 1024).toFixed(1).replace(/\.0$/, "").replace(".", ",")} GB`;

function showMessage(msg: string, title = "Não foi possível continuar") {
  $("error-title").textContent = title;
  $("error-text").textContent = msg;
  show($("error-overlay"));
  $("error-close").focus();
}

async function copyText(text: string, btn?: HTMLElement) {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    const ta = document.createElement("textarea");
    ta.value = text;
    document.body.appendChild(ta);
    ta.select();
    document.execCommand("copy");
    ta.remove();
  }
  if (btn) {
    const old = btn.textContent;
    btn.textContent = "Copiado";
    setTimeout(() => (btn.textContent = old), 1400);
  }
}

function setBanner(msg: string | null) {
  $("banner").textContent = msg ?? "";
  show($("banner"), !!msg);
}

// ---------- console ----------
function pushLog(line: string) {
  logLines.push(line);
  if (logLines.length > MAX_LOG) logLines.splice(0, logLines.length - MAX_LOG);
  logDirty = true;
}
function renderConsole() {
  const pre = $("tab-console");
  if (!logDirty || pre.classList.contains("hidden")) return;
  logDirty = false;
  const stick = pre.scrollTop + pre.clientHeight >= pre.scrollHeight - 40;
  pre.textContent = logLines.length ? logLines.slice(-1000).join("\n") : "O log do jogo aparece aqui quando você clicar em Jogar.";
  if (stick) pre.scrollTop = pre.scrollHeight;
}
setInterval(renderConsole, 300);

function selectTab(name: string) {
  document.querySelectorAll<HTMLElement>(".tab").forEach((t) => {
    const on = t.dataset.tab === name;
    t.classList.toggle("active", on);
    t.setAttribute("aria-selected", String(on));
  });
  show($("tab-news"), name === "news");
  show($("tab-console"), name === "console");
  show($("btn-copy-log"), name === "console");
  if (name === "console") {
    logDirty = true;
    renderConsole();
    $("tab-console").scrollTop = $("tab-console").scrollHeight;
  }
}

// ---------- progresso em blocos ----------
function buildBlocks() {
  const el = $("blocks");
  el.innerHTML = "";
  for (let i = 0; i < BLOCKS; i++) el.appendChild(document.createElement("i"));
}
function setBlocks(fraction: number | null) {
  const el = $("blocks");
  el.classList.toggle("busy", fraction === null);
  const lit = fraction === null ? 0 : Math.round(Math.min(1, Math.max(0, fraction)) * BLOCKS);
  el.querySelectorAll("i").forEach((b, i) => {
    b.classList.toggle("on", i < lit);
    // Pulso escalonado só no modo "sem total conhecido".
    (b as HTMLElement).style.animationDelay = fraction === null ? `${(i % BLOCKS) * 0.08}s` : "";
  });
  if (fraction === null) el.removeAttribute("aria-valuenow");
  else el.setAttribute("aria-valuenow", String(Math.round(fraction * 100)));
}
function setDetail(parts: string[]) {
  $("progress-detail").innerHTML = parts.map((p) => `<span>${p}</span>`).join("");
}
function setStage(label: string) {
  show($("progress"));
  $("progress-stage").textContent = label.replace(/\.\.\.$/, "");
  setBlocks(null);
  setDetail([]);
}
function onProgress(e: ProgressEvent) {
  show($("progress"));
  const parts: string[] = [];
  if (e.totalBytes > 0) {
    const f = e.doneBytes / e.totalBytes;
    setBlocks(f);
    parts.push(`<b>${Math.min(100, Math.floor(f * 100))}%</b>`, `${formatBytes(e.doneBytes)} de ${formatBytes(e.totalBytes)}`);
  } else {
    setBlocks(null);
    if (e.doneBytes > 0) parts.push(formatBytes(e.doneBytes));
  }
  if (e.filesTotal > 1) parts.push(`${e.filesDone} de ${e.filesTotal} arquivos`);
  if (e.speedBps > 0) parts.push(`${formatBytes(e.speedBps)}/s`);
  if (e.etaSecs != null && e.etaSecs > 0) parts.push(`faltam ${formatEta(e.etaSecs)}`);
  setDetail(parts);
}

function setBusy(on: boolean) {
  busy = on;
  const b = $<HTMLButtonElement>("btn-play");
  $("play-label").textContent = playing ? "Fechar jogo" : on ? "Aguarde" : "Jogar";
  b.classList.toggle("stop", playing);
  b.disabled = on && !playing;
  $<HTMLInputElement>("nick").disabled = on;
}

// ---------- eventos do backend ----------
function onEvent(e: LauncherEvent) {
  switch (e.type) {
    case "stage":
      setStage(e.label);
      break;
    case "progress":
      onProgress(e);
      break;
    case "warning":
      setBanner(e.message);
      break;
    case "log":
      pushLog(e.stream === "installer" ? `[instalador] ${e.line}` : e.line);
      break;
    case "gameStarted":
      playing = true;
      show($("progress"), false);
      setBusy(true);
      pushLog("=== Jogo iniciado ===");
      break;
    case "gameExit":
      playing = false;
      setBusy(false);
      pushLog(`=== Jogo encerrado (código ${e.code ?? "?"}) ===`);
      if (e.crashed) {
        $("crash-summary").textContent = e.summary ?? "O jogo fechou inesperadamente.";
        show($("crash-overlay"));
      }
      void refreshAll();
      break;
  }
}

// ---------- novidades e status ----------
let infoError = "";

function renderNews() {
  const el = $("tab-news");
  if (!pack) {
    const notPublished = infoError.includes("não tem um modpack publicado");
    const title = notPublished ? "Nenhum modpack publicado ainda" : "Sem conexão com o servidor de atualizações";
    const body = notPublished
      ? "O servidor de atualizações está no ar, mas o administrador ainda não publicou o modpack."
      : `${escapeHtml(infoError || "Não consegui falar com o servidor.")} As novidades aparecem aqui assim que a conexão voltar.`;
    el.innerHTML = `<div class="empty"><strong>${title}</strong>${body}</div>`;
  } else if (!pack.news.trim()) {
    el.innerHTML = `<div class="empty"><strong>Nada novo por enquanto</strong>Avisos de eventos e atualizações do servidor aparecem aqui.</div>`;
  } else {
    el.innerHTML = renderMarkdown(pack.news);
  }
}

// Rostinho em bloco 9x9 (contorno escuro + relevo, igual ao botão Jogar).
type Mood = "on" | "off" | "idle";
const FACE: Record<Mood, { base: string; hi: string; lo: string; ink: string; mouth: [number, number][] }> = {
  on: { base: "#57c78a", hi: "#86e3ae", lo: "#2f8a57", ink: "#10351f", mouth: [[2, 5], [6, 5], [3, 6], [4, 6], [5, 6]] }, // sorrindo
  off: { base: "#e5534b", hi: "#f27f78", lo: "#9e2c26", ink: "#3a0d0b", mouth: [[3, 5], [4, 5], [5, 5], [2, 6], [6, 6]] }, // triste
  idle: { base: "#6d778b", hi: "#8b94a7", lo: "#4b5466", ink: "#1f2430", mouth: [[3, 6], [4, 6], [5, 6]] }, // neutro
};
function setFace(mood: Mood) {
  const f = FACE[mood];
  const px: string[] = [];
  const rect = (x: number, y: number, c: string) => px.push(`<rect x="${x}" y="${y}" width="1" height="1" fill="${c}"/>`);
  for (let y = 0; y < 9; y++)
    for (let x = 0; x < 9; x++) {
      if (x === 0 || y === 0 || x === 8 || y === 8) rect(x, y, "#0f131b");
      else rect(x, y, y === 7 || x === 7 ? f.lo : y === 1 || x === 1 ? f.hi : f.base);
    }
  for (const [x, y] of [[3, 3], [5, 3], ...f.mouth] as [number, number][]) rect(x, y, f.ink);
  const el = $("srv-dot");
  el.className = `face ${mood}`;
  el.innerHTML = `<svg viewBox="0 0 9 9" shape-rendering="crispEdges">${px.join("")}</svg>`;
}

async function refreshStatus() {
  const addr = pack?.serverAddress;
  if (!addr) {
    setFace("idle");
    $("srv-title").textContent = "Servidor desconhecido";
    $("srv-motd").textContent = "Conecte-se à internet uma vez para o launcher descobrir o servidor.";
    $("srv-players").textContent = "";
    return;
  }
  const s = await invoke<ServerStatus>("server_status", { address: addr });
  setFace(s.online ? "on" : "off");
  $("srv-title").textContent = s.online ? "Servidor online" : "Servidor offline";
  $("srv-motd").textContent = s.online ? s.motd || `Minecraft ${s.version}` : "O servidor não respondeu, provavelmente está offline.";
  $("srv-players").innerHTML = s.online ? `<strong>${s.playersOnline}</strong> de ${s.playersMax} jogando` : "";
}

async function refreshAll() {
  try {
    pack = await invoke<PackInfo | null>("refresh_info");
    infoError = "";
  } catch (e) {
    pack = null;
    infoError = errText(e);
  }
  renderNews();
  setBanner(pack?.offline ? "Sem conexão com o servidor de atualizações. Você vai jogar com a última versão instalada." : null);
  await refreshStatus();
}

// ---------- Jogar ----------
function readNick(): string | null {
  const n = $<HTMLInputElement>("nick").value.trim();
  const err = $("nick-error");
  if (!NICK_RE.test(n)) {
    err.textContent = n.length === 0 ? "Digite um nome para jogar." : "Use de 3 a 16 caracteres: letras sem acento, números ou _ (sem espaços).";
    show(err);
    $("nick").setAttribute("aria-invalid", "true");
    $("nick").focus();
    return null;
  }
  show(err, false);
  $("nick").removeAttribute("aria-invalid");
  return n;
}

async function onPlay() {
  if (playing) {
    await invoke("stop_game");
    return;
  }
  if (busy) return;
  const nick = readNick();
  if (!nick) return;
  settings = { ...settings, nickname: nick };
  try {
    await invoke("save_settings", { new: settings });
  } catch (e) {
    showMessage(errText(e));
    return;
  }
  setBusy(true);
  setStage("Preparando");
  try {
    await invoke("play");
  } catch (e) {
    playing = false;
    setBusy(false);
    show($("progress"), false);
    showMessage(errText(e));
  }
}

// ---------- configurações ----------
function openSettings() {
  const s = settings;
  const maxRam = Math.max(2048, Math.min(Math.floor((init.totalRamMb * 0.85) / 256) * 256, 24576));
  const ram = $<HTMLInputElement>("ram");
  ram.min = "1024";
  ram.max = String(maxRam);
  const auto = s.ramMb === 0;
  $<HTMLInputElement>("ram-auto").checked = auto;
  ram.value = String(auto ? init.defaultRamMb : Math.min(s.ramMb, maxRam));
  ram.disabled = auto;
  updateRamLabel();
  const min = pack ? `O modpack recomenda pelo menos ${gb(pack.minRamMb)}. ` : "";
  $("ram-hint").textContent = `${min}Seu computador tem ${gb(init.totalRamMb)}.`;
  $<HTMLInputElement>("res-w").value = s.width ? String(s.width) : "";
  $<HTMLInputElement>("res-h").value = s.height ? String(s.height) : "";
  $<HTMLInputElement>("quickplay").checked = s.quickPlay;
  $<HTMLInputElement>("gamedir").value = s.gameDir;
  $("gamedir-hint").textContent = `Em uso: ${init.gameRoot}`;
  $<HTMLTextAreaElement>("jvm").value = s.jvmArgs;
  for (const id of ["btn-repair", "btn-clear"]) $<HTMLButtonElement>(id).disabled = busy;
  show($("settings-error"), false);
  show($("settings-overlay"));
}
function updateRamLabel() {
  $("ram-value").textContent = gb(Number($<HTMLInputElement>("ram").value));
}

async function saveSettings() {
  const num = (id: string) => Number($<HTMLInputElement>(id).value.trim() || 0);
  const next: Settings = {
    ...settings,
    ramMb: $<HTMLInputElement>("ram-auto").checked ? 0 : Number($<HTMLInputElement>("ram").value),
    width: num("res-w"),
    height: num("res-h"),
    quickPlay: $<HTMLInputElement>("quickplay").checked,
    gameDir: $<HTMLInputElement>("gamedir").value.trim(),
    jvmArgs: $<HTMLTextAreaElement>("jvm").value.trim(),
  };
  try {
    await invoke("save_settings", { new: next });
    settings = next;
    init = await invoke<InitInfo>("get_init");
    show($("settings-overlay"), false);
  } catch (e) {
    $("settings-error").textContent = errText(e);
    show($("settings-error"));
  }
}

async function runMaintenance(cmd: "repair" | "clear_cache") {
  setBusy(true);
  show($("settings-overlay"), false);
  setStage(cmd === "repair" ? "Reparando a instalação" : "Limpando o cache");
  try {
    const r = await invoke<number | null>(cmd);
    show($("progress"), false);
    if (cmd === "repair") showMessage("Tudo foi verificado e o que estava errado foi baixado de novo.", "Instalação reparada");
    else showMessage(`${formatBytes(Number(r ?? 0))} liberados.`, "Cache limpo");
  } catch (e) {
    show($("progress"), false);
    showMessage(errText(e));
  } finally {
    setBusy(false);
  }
}

// ---------- atualização do launcher ----------
async function autoUpdate() {
  try {
    const u = await invoke<{ version: string; notes: string } | null>("check_update");
    if (!u) return;
    $("update-text").innerHTML = `Instalando a versão <strong>${escapeHtml(u.version)}</strong>. O launcher reabre sozinho em seguida.`;
    show($("update-overlay"));
    await invoke("install_update");
  } catch (e) {
    show($("update-overlay"), false);
    setBanner(`Não deu para atualizar o launcher agora: ${errText(e)}`);
  }
}

// ---------- inicialização ----------
function closeTopOverlay() {
  for (const id of ["settings-overlay", "crash-overlay", "error-overlay"]) {
    const el = $(id);
    if (!el.classList.contains("hidden")) {
      show(el, false);
      return;
    }
  }
}

async function main() {
  buildBlocks();
  setFace("idle");
  drawTerrain($<HTMLCanvasElement>("terrain"));

  init = await invoke<InitInfo>("get_init");
  settings = init.settings;
  document.title = init.name;
  $("launcher-name").textContent = init.name;
  $<HTMLInputElement>("nick").value = settings.nickname;
  setBusy(false);
  if (!settings.nickname) $("nick").focus();

  await listen<LauncherEvent>("launcher-event", (ev) => {
    const p = ev.payload;
    if (p.type === "progress" && p.label === UPDATE_LABEL) {
      if (p.totalBytes > 0) $("update-fill").style.width = `${Math.min(100, (p.doneBytes / p.totalBytes) * 100)}%`;
      return;
    }
    onEvent(p);
  });

  const win = getCurrentWindow();
  $("btn-min").addEventListener("click", () => void win.minimize());
  $("btn-close").addEventListener("click", () => void win.close());

  document.querySelectorAll<HTMLElement>(".tab").forEach((t) => t.addEventListener("click", () => selectTab(t.dataset.tab!)));
  $("btn-play").addEventListener("click", () => void onPlay());
  $("nick").addEventListener("keydown", (e) => e.key === "Enter" && void onPlay());
  $("nick").addEventListener("input", () => {
    show($("nick-error"), false);
    $("nick").removeAttribute("aria-invalid");
  });
  $("btn-settings").addEventListener("click", openSettings);
  $("settings-cancel").addEventListener("click", () => show($("settings-overlay"), false));
  $("settings-save").addEventListener("click", () => void saveSettings());
  $("ram").addEventListener("input", updateRamLabel);
  $("ram-auto").addEventListener("change", () => {
    const auto = $<HTMLInputElement>("ram-auto").checked;
    $<HTMLInputElement>("ram").disabled = auto;
    if (auto) $<HTMLInputElement>("ram").value = String(init.defaultRamMb);
    updateRamLabel();
  });
  $("jvm-reset").addEventListener("click", () => ($<HTMLTextAreaElement>("jvm").value = init.defaultJvmArgs));
  const openDir = () => void invoke("open_game_dir").catch((e) => showMessage(errText(e)));
  $("btn-open-dir").addEventListener("click", openDir);
  $("crash-folder").addEventListener("click", openDir);
  $("btn-repair").addEventListener("click", () => void runMaintenance("repair"));
  $("btn-clear").addEventListener("click", () => void runMaintenance("clear_cache"));
  $("btn-copy-log").addEventListener("click", (e) => void copyText(logLines.join("\n"), e.currentTarget as HTMLElement));
  $("crash-copy").addEventListener("click", (e) => void copyText(logLines.join("\n"), e.currentTarget as HTMLElement));
  $("crash-close").addEventListener("click", () => show($("crash-overlay"), false));
  $("error-close").addEventListener("click", () => show($("error-overlay"), false));
  $("error-copy").addEventListener("click", (e) => void copyText($("error-text").textContent ?? "", e.currentTarget as HTMLElement));
  document.addEventListener("keydown", (e) => e.key === "Escape" && closeTopOverlay());
  // Links das novidades abrem no navegador (a janela do launcher nunca navega).
  document.addEventListener("click", (ev) => {
    const a = (ev.target as HTMLElement).closest<HTMLAnchorElement>("a[data-ext]");
    if (!a) return;
    ev.preventDefault();
    void invoke("open_url", { url: a.getAttribute("href") }).catch(() => {});
  });
  document.addEventListener("contextmenu", (e) => e.preventDefault());

  await refreshAll();
  setInterval(() => !playing && void refreshStatus(), 30_000);
  void autoUpdate();
}

main().catch((e) => showMessage(`O launcher não conseguiu iniciar: ${errText(e)}`));
