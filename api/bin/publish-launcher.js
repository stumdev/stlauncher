#!/usr/bin/env node
// Publica uma nova versão do launcher (instalador NSIS + assinatura .sig do Tauri).
// Uso: node bin/publish-launcher.js <instalador.exe> --version 1.0.1 [--notes "..."] [--sig arquivo.sig]
//      [--remote URL --token T]
import { copyFile, readFile, stat } from 'node:fs/promises';
import { createReadStream } from 'node:fs';
import path from 'node:path';
import { Readable } from 'node:stream';
import { parseArgs } from 'node:util';
import { loadConfig } from '../src/config.js';
import { writeJsonAtomic } from '../src/lib/store.js';

const { values, positionals } = parseArgs({
  allowPositionals: true,
  options: {
    version: { type: 'string' },
    notes: { type: 'string' },
    sig: { type: 'string' },
    remote: { type: 'string' },
    token: { type: 'string' },
  },
});

try {
  const installer = positionals[0];
  if (!installer || !values.version) throw new Error('Uso: publish-launcher <instalador.exe> --version X.Y.Z [--notes "..."]');
  if (!/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(values.version)) throw new Error('--version inválida (ex: 1.0.1)');
  await stat(installer);
  const sigFile = values.sig || `${installer}.sig`;
  const signature = (await readFile(sigFile, 'utf8')).trim();
  const file = path.basename(installer).replace(/[^A-Za-z0-9._-]/g, '_');
  const notes = values.notes || '';

  if (values.remote) {
    const remote = values.remote.replace(/\/+$/, '');
    const token = values.token || process.env.ADMIN_TOKEN;
    if (!token) throw new Error('Informe --token ou defina ADMIN_TOKEN');
    const h = { 'x-admin-token': token };
    let r = await fetch(`${remote}/api/admin/launcher-files/${encodeURIComponent(file)}`, {
      method: 'PUT',
      headers: { ...h, 'content-type': 'application/octet-stream' },
      body: Readable.toWeb(createReadStream(installer)),
      duplex: 'half',
    });
    if (!r.ok) throw new Error(`Upload falhou: HTTP ${r.status} ${await r.text()}`);
    r = await fetch(`${remote}/api/admin/launcher`, {
      method: 'POST',
      headers: { ...h, 'content-type': 'application/json' },
      body: JSON.stringify({ version: values.version, notes, file, signature }),
    });
    if (!r.ok) throw new Error(`Publicação falhou: HTTP ${r.status} ${await r.text()}`);
  } else {
    const dir = loadConfig().launcherDir;
    await copyFile(installer, path.join(dir, 'files', file)).catch((e) => {
      throw new Error(`Não consegui copiar para ${dir}/files (${e.code}). A pasta existe? Inicie a API uma vez.`);
    });
    await writeJsonAtomic(path.join(dir, 'launcher.json'), {
      version: values.version, notes, file, signature, pub_date: new Date().toISOString(),
    });
  }
  console.log(`Launcher ${values.version} publicado (${file}).`);
} catch (e) {
  console.error(`Erro: ${e.message}`);
  process.exit(1);
}
