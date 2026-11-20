// Lógica do `publish-pack`: sincroniza uma pasta local de pack com o servidor (local ou remoto).
import { copyFile, mkdir, rename } from 'node:fs/promises';
import { createReadStream } from 'node:fs';
import path from 'node:path';
import { Readable } from 'node:stream';
import { encodePathForUrl } from './paths.js';
import { scanFiles } from './store.js';

/** Diferença entre duas listas [{path, sha256}]: o que enviar e o que apagar. */
export function diffFiles(src, dest) {
  const d = new Map(dest.map((f) => [f.path, f.sha256]));
  const s = new Set(src.map((f) => f.path));
  return {
    upload: src.filter((f) => d.get(f.path) !== f.sha256),
    remove: dest.filter((f) => !s.has(f.path)).map((f) => f.path),
  };
}

/** Modo local: copia para <packDir>/files e regenera o manifest. */
export async function publishLocal(srcDir, store, meta, log = console.log) {
  const { files: src, skipped } = await scanFiles(srcDir);
  await store.init();
  const { files: dest } = await scanFiles(store.filesDir);
  const { upload, remove } = diffFiles(src, dest);
  for (const f of upload) {
    const to = store.absPath(f.path);
    await mkdir(path.dirname(to), { recursive: true });
    const tmp = `${to}.${process.pid}.tmp`;
    await copyFile(path.join(srcDir, ...f.path.split('/')), tmp);
    await rename(tmp, to);
    log(`  + ${f.path}`);
  }
  for (const p of remove) {
    await store.removeFile(p);
    log(`  - ${p}`);
  }
  const result = await store.publish(meta);
  return { ...result, skippedSource: skipped, uploaded: upload.length, removed: remove.length };
}

async function api(remote, token, method, route, { body, headers = {}, duplex } = {}) {
  const res = await fetch(`${remote}${route}`, {
    method,
    headers: { 'x-admin-token': token, ...headers },
    body,
    duplex,
  });
  const text = await res.text();
  let json;
  try {
    json = JSON.parse(text);
  } catch {
    /* corpo não-JSON */
  }
  if (!res.ok) throw new Error(`${method} ${route} -> ${res.status}: ${json?.error || text.slice(0, 200)}`);
  return json;
}

/** Modo remoto: envia só o que mudou via /api/admin e pede para o servidor republicar. */
export async function publishRemote(srcDir, remote, token, meta, log = console.log) {
  remote = remote.replace(/\/+$/, '');
  const { files: src, skipped } = await scanFiles(srcDir);
  let dest = [];
  const res = await fetch(`${remote}/api/pack/manifest`);
  if (res.ok) dest = (await res.json()).files;
  else if (res.status !== 404) throw new Error(`Falha ao consultar o servidor: HTTP ${res.status}`);
  const { upload, remove } = diffFiles(src, dest);
  for (const f of upload) {
    const body = Readable.toWeb(createReadStream(path.join(srcDir, ...f.path.split('/'))));
    await api(remote, token, 'PUT', `/api/admin/files/${encodePathForUrl(f.path)}`, {
      body,
      duplex: 'half',
      headers: { 'content-type': 'application/octet-stream', 'x-sha256': f.sha256 },
    });
    log(`  + ${f.path}`);
  }
  for (const p of remove) {
    await api(remote, token, 'DELETE', `/api/admin/files/${encodePathForUrl(p)}`);
    log(`  - ${p}`);
  }
  const r = await api(remote, token, 'POST', '/api/admin/publish', {
    body: JSON.stringify(meta),
    headers: { 'content-type': 'application/json' },
  });
  return { ...r, skippedSource: skipped, uploaded: upload.length, removed: remove.length };
}
