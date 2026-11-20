// Armazenamento do pack em disco: files/ (os arquivos), manifest.json (gerado) e meta.json (config).
import { createHash } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { mkdir, readdir, readFile, rename, stat, unlink, writeFile, rmdir } from 'node:fs/promises';
import path from 'node:path';
import { ALLOWED_FILES, ALLOWED_ROOTS, encodePathForUrl, validateRelPath } from './paths.js';

export const META_DEFAULTS = {
  mcVersion: '1.21.1',
  neoforgeVersion: '', // obrigatório: ex. 21.1.172
  serverAddress: 'meuservidor.exemplo.com:25565', // PLACEHOLDER
  minRamMb: 4096,
  news: '',
};

export function validateMeta(m) {
  const errs = [];
  if (!/^\d+\.\d+(\.\d+)?$/.test(m.mcVersion)) errs.push('mcVersion inválido (ex: 1.21.1)');
  if (!/^\d+\.\d+\.\d+(-[A-Za-z0-9.]+)?$/.test(m.neoforgeVersion))
    errs.push('neoforgeVersion inválido ou ausente (ex: 21.1.172)');
  if (!/^[A-Za-z0-9.\-]+(:\d{1,5})?$/.test(m.serverAddress)) errs.push('serverAddress inválido (ex: host:25565)');
  if (!Number.isInteger(m.minRamMb) || m.minRamMb < 1024 || m.minRamMb > 65536)
    errs.push('minRamMb deve ser inteiro entre 1024 e 65536');
  if (typeof m.news !== 'string' || m.news.length > 5000) errs.push('news deve ser texto de até 5000 caracteres');
  if (errs.length) throw Object.assign(new Error(errs.join('; ')), { statusCode: 400 });
}

/** Calcula sha256 de um arquivo em streaming. */
export function sha256File(file) {
  return new Promise((resolve, reject) => {
    const h = createHash('sha256');
    createReadStream(file)
      .on('data', (c) => h.update(c))
      .on('error', reject)
      .on('end', () => resolve(h.digest('hex')));
  });
}

/**
 * Varre `dir` e devolve os arquivos válidos do pack: [{path, sha256, size}].
 * `cache` (Map path -> {size, mtimeMs, sha256}) evita recalcular hash de arquivo inalterado.
 * Itens fora da whitelist ou com nome inválido vão para `skipped`.
 */
export async function scanFiles(dir, cache = new Map()) {
  const files = [];
  const skipped = [];
  async function walk(abs, rel) {
    for (const ent of await readdir(abs, { withFileTypes: true })) {
      const childRel = rel ? `${rel}/${ent.name}` : ent.name;
      const childAbs = path.join(abs, ent.name);
      if (ent.isDirectory()) {
        // Ignora diretórios de topo fora da whitelist sem varrer.
        if (!rel && !ALLOWED_ROOTS.includes(ent.name)) {
          skipped.push(childRel + '/');
          continue;
        }
        await walk(childAbs, childRel);
      } else if (ent.isFile()) {
        if (!rel && !ALLOWED_FILES.includes(ent.name)) {
          skipped.push(childRel);
          continue;
        }
        try {
          validateRelPath(childRel);
        } catch {
          skipped.push(childRel);
          continue;
        }
        const st = await stat(childAbs);
        const c = cache.get(childRel);
        let sha;
        if (c && c.size === st.size && c.mtimeMs === st.mtimeMs) sha = c.sha256;
        else sha = await sha256File(childAbs);
        cache.set(childRel, { size: st.size, mtimeMs: st.mtimeMs, sha256: sha });
        files.push({ path: childRel, sha256: sha, size: st.size });
      } else {
        skipped.push(childRel);
      }
    }
  }
  await walk(dir, '');
  files.sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0));
  // Windows é case-insensitive: dois caminhos que só diferem em maiúsculas colidiriam.
  const seen = new Set();
  for (const f of files) {
    const k = f.path.toLowerCase();
    if (seen.has(k)) throw Object.assign(new Error(`caminhos duplicados (maiúsc./minúsc.): ${f.path}`), { statusCode: 400 });
    seen.add(k);
  }
  return { files, skipped };
}

async function readJson(file) {
  try {
    return JSON.parse(await readFile(file, 'utf8'));
  } catch (e) {
    if (e.code === 'ENOENT') return null;
    throw e;
  }
}

/** Escrita atômica: grava em .tmp e renomeia. */
export async function writeJsonAtomic(file, data) {
  const tmp = `${file}.${process.pid}.tmp`;
  await writeFile(tmp, JSON.stringify(data, null, 2));
  await rename(tmp, file);
}

export function autoVersion(d = new Date()) {
  const p = (n, l = 2) => String(n).padStart(l, '0');
  return `${d.getUTCFullYear()}.${p(d.getUTCMonth() + 1)}.${p(d.getUTCDate())}.${p(d.getUTCHours())}${p(d.getUTCMinutes())}`;
}

export class PackStore {
  constructor(packDir) {
    this.dir = packDir;
    this.filesDir = path.join(packDir, 'files');
    this.manifestPath = path.join(packDir, 'manifest.json');
    this.metaPath = path.join(packDir, 'meta.json');
    this.cachePath = path.join(packDir, '.hashcache.json');
    this._lock = Promise.resolve();
  }

  async init() {
    await mkdir(this.filesDir, { recursive: true });
  }

  readManifest() {
    return readJson(this.manifestPath);
  }

  async readMeta() {
    return { ...META_DEFAULTS, ...((await readJson(this.metaPath)) || {}) };
  }

  /** Caminho absoluto seguro dentro de files/ para um caminho relativo já validado. */
  absPath(rel) {
    validateRelPath(rel);
    const abs = path.resolve(this.filesDir, ...rel.split('/'));
    if (!abs.startsWith(this.filesDir + path.sep)) throw new Error('caminho fora da pasta do pack');
    return abs;
  }

  async removeFile(rel) {
    const abs = this.absPath(rel);
    await unlink(abs);
    // Remove pastas vazias para cima, sem passar de files/.
    let d = path.dirname(abs);
    while (d.startsWith(this.filesDir + path.sep)) {
      try {
        await rmdir(d);
      } catch {
        break;
      }
      d = path.dirname(d);
    }
  }

  /** Reescaneia files/ e regenera o manifest. Serializado para evitar corrida. */
  publish(patch = {}) {
    const run = () => this._publish(patch);
    const p = this._lock.then(run, run);
    this._lock = p.catch(() => {});
    return p;
  }

  async _publish(patch) {
    const stored = await this.readMeta();
    const clean = {};
    for (const k of Object.keys(META_DEFAULTS)) if (patch[k] !== undefined) clean[k] = patch[k];
    const meta = { ...stored, ...clean };
    validateMeta(meta);

    const cacheRaw = (await readJson(this.cachePath)) || {};
    const cache = new Map(Object.entries(cacheRaw));
    const { files, skipped } = await scanFiles(this.filesDir, cache);
    const prev = await this.readManifest();

    const sig = (list) => list.map((f) => `${f.path}:${f.sha256}`).join('\n');
    const changed = !prev || sig(prev.files) !== sig(files);
    const packVersion = patch.packVersion || (changed ? autoVersion() : prev.packVersion);

    const manifest = {
      schema: 1,
      packVersion,
      generatedAt: new Date().toISOString(),
      mcVersion: meta.mcVersion,
      neoforgeVersion: meta.neoforgeVersion,
      serverAddress: meta.serverAddress,
      minRamMb: meta.minRamMb,
      news: meta.news,
      files: files.map((f) => ({ ...f, url: `/files/${encodePathForUrl(f.path)}` })),
    };
    await writeJsonAtomic(this.metaPath, meta);
    await writeJsonAtomic(this.manifestPath, manifest);
    await writeJsonAtomic(this.cachePath, Object.fromEntries(cache));
    return { manifest, skipped, changed };
  }
}
