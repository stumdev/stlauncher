// Desenha uma faixa de terreno em blocos (grama, terra, pedra e minérios) no rodapé da coluna esquerda.
// Gerado por código (semente fixa): nenhuma imagem extra no .exe.

const PX = 2; // pixels CSS por "pixel de textura"
const TEX = 8; // pixels de textura por bloco (bloco = 16px CSS)

function rng(seed: number) {
  return () => {
    seed = (seed * 1664525 + 1013904223) >>> 0;
    return seed / 4294967296;
  };
}

type RGB = [number, number, number];
const hex = (h: string): RGB => [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16)) as RGB;
const PALETTE = {
  grass: [hex("#5b8a3c"), hex("#4f7a34"), hex("#6a9a46")],
  dirt: [hex("#6b4a33"), hex("#5d402c"), hex("#7a563b")],
  stone: [hex("#3d4454"), hex("#363c4b"), hex("#444c5d")],
  deep: [hex("#2a2f3d"), hex("#252a36"), hex("#2f3544")],
  gold: [hex("#f4b942"), hex("#ffd977")],
  emerald: [hex("#57c78a"), hex("#86e3ae")],
};

export function drawTerrain(canvas: HTMLCanvasElement) {
  const cssW = canvas.clientWidth;
  const cssH = canvas.clientHeight;
  const w = Math.ceil(cssW / PX);
  const h = Math.ceil(cssH / PX);
  canvas.width = w;
  canvas.height = h;
  const ctx = canvas.getContext("2d");
  if (!ctx) return;
  const img = ctx.createImageData(w, h);
  const rand = rng(20241121);
  const cols = Math.ceil(w / TEX);
  const rows = Math.ceil(h / TEX);

  // Relevo suave: altura da superfície (em blocos a partir do fundo) por coluna.
  const heights: number[] = [];
  let cur = 4;
  for (let c = 0; c < cols; c++) {
    const r = rand();
    if (r < 0.22) cur = Math.min(rows - 2, cur + 1);
    else if (r > 0.8) cur = Math.max(3, cur - 1);
    heights.push(cur);
  }

  const put = (x: number, y: number, [r, g, b]: RGB, a = 255) => {
    if (x < 0 || y < 0 || x >= w || y >= h) return;
    const i = (y * w + x) * 4;
    img.data[i] = r;
    img.data[i + 1] = g;
    img.data[i + 2] = b;
    img.data[i + 3] = a;
  };
  const pick = (list: RGB[]) => list[Math.floor(rand() * list.length)];

  for (let c = 0; c < cols; c++) {
    const surface = heights[c];
    for (let rowFromBottom = 0; rowFromBottom < surface; rowFromBottom++) {
      const depth = surface - 1 - rowFromBottom; // 0 = topo
      const by = h - (rowFromBottom + 1) * TEX;
      const bx = c * TEX;
      const base = depth === 0 ? "grass" : depth <= 1 ? "dirt" : depth <= 2 ? "stone" : "deep";
      const ore = base !== "grass" && base !== "dirt" && rand() < 0.1 ? (rand() < 0.75 ? "gold" : "emerald") : null;
      for (let y = 0; y < TEX; y++) {
        for (let x = 0; x < TEX; x++) {
          let col: RGB;
          if (base === "grass") col = y < 3 || (y === 3 && rand() < 0.5) ? pick(PALETTE.grass) : pick(PALETTE.dirt);
          else col = pick(PALETTE[base]);
          put(bx + x, by + y, col);
        }
      }
      if (ore) {
        // 3 a 4 pontinhos de minério dentro do bloco
        const n = 3 + Math.floor(rand() * 2);
        for (let k = 0; k < n; k++) {
          const ox = 1 + Math.floor(rand() * (TEX - 3));
          const oy = 1 + Math.floor(rand() * (TEX - 3));
          const p = PALETTE[ore];
          put(bx + ox, by + oy, p[0]);
          put(bx + ox + 1, by + oy, p[1]);
          put(bx + ox, by + oy + 1, p[0]);
        }
      }
    }
  }
  ctx.putImageData(img, 0, 0);

  // Escurece levemente para baixo, para o terreno não competir com o botão.
  const g = ctx.createLinearGradient(0, h - rows * TEX, 0, h);
  g.addColorStop(0, "rgba(15,19,27,0)");
  g.addColorStop(1, "rgba(15,19,27,0.55)");
  ctx.globalCompositeOperation = "source-atop";
  ctx.fillStyle = g;
  ctx.fillRect(0, 0, w, h);
}
