// Mini renderizador de markdown para as notícias vindas da API.
// Seguro por construção: TODO o texto é escapado antes de virar HTML; só geramos as tags abaixo.
// Suporta: # títulos, listas "- ", **negrito**, *itálico*, `código`, [texto](http(s)://link), parágrafos.

export function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

/** Formatação inline sobre texto JÁ escapado. */
function inline(escaped: string): string {
  // Separa `código` para não formatar dentro dele.
  return escaped
    .split(/(`[^`]+`)/g)
    .map((part) => {
      if (/^`[^`]+`$/.test(part)) return `<code>${part.slice(1, -1)}</code>`;
      return part
        .replace(/\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/g, '<a href="$2" data-ext="1">$1</a>')
        .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
        .replace(/(^|[^*])\*([^*\s][^*]*)\*(?!\*)/g, "$1<em>$2</em>");
    })
    .join("");
}

export function renderMarkdown(src: string): string {
  const out: string[] = [];
  let list: string[] = [];
  let para: string[] = [];
  const flushList = () => {
    if (list.length) out.push(`<ul>${list.map((l) => `<li>${l}</li>`).join("")}</ul>`);
    list = [];
  };
  const flushPara = () => {
    if (para.length) out.push(`<p>${para.join("<br>")}</p>`);
    para = [];
  };
  for (const raw of src.replace(/\r\n?/g, "\n").split("\n")) {
    const line = escapeHtml(raw.trimEnd());
    let m: RegExpMatchArray | null;
    if (line.trim() === "") {
      flushList();
      flushPara();
    } else if ((m = line.match(/^(#{1,3})\s+(.*)$/))) {
      flushList();
      flushPara();
      out.push(`<h${m[1].length + 2}>${inline(m[2])}</h${m[1].length + 2}>`);
    } else if ((m = line.match(/^\s*[-*]\s+(.*)$/))) {
      flushPara();
      list.push(inline(m[1]));
    } else {
      flushList();
      para.push(inline(line));
    }
  }
  flushList();
  flushPara();
  return out.join("");
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const u = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(v >= 100 ? 0 : 1)} ${u[i]}`;
}

export function formatEta(secs: number): string {
  if (secs < 60) return `${Math.max(1, Math.round(secs))}s`;
  const m = Math.floor(secs / 60);
  if (m < 60) return `${m}min ${Math.round(secs % 60)}s`;
  return `${Math.floor(m / 60)}h ${m % 60}min`;
}
