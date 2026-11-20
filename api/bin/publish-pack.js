#!/usr/bin/env node
// Publica o pack: node bin/publish-pack.js <pasta-do-pack> [opções]
import { parseArgs } from 'node:util';
import { readFile } from 'node:fs/promises';
import { loadConfig } from '../src/config.js';
import { publishLocal, publishRemote } from '../src/lib/publish.js';
import { PackStore } from '../src/lib/store.js';

const HELP = `Uso: publish-pack [pasta-do-pack] [opções]

  pasta-do-pack        pasta com mods/, config/, resourcepacks/, shaderpacks/, servers.dat
                       (omita para só atualizar notícias/metadados e regenerar o manifest)
Opções:
  --remote <url>       envia para uma API remota (usa --token ou env ADMIN_TOKEN)
  --token <token>      token de admin (modo remoto)
  --neoforge <versão>  versão do NeoForge (ex: 21.1.172)
  --mc <versão>        versão do Minecraft (padrão: 1.21.1)
  --server <host:porta> endereço do servidor de Minecraft
  --min-ram <MB>       RAM mínima recomendada
  --news <texto>       texto/markdown das notícias        (--news-file <arquivo> lê de arquivo)
  --pack-version <v>   força a versão do pack (padrão: automática)
Sem --remote, publica na pasta local da API (PACK_DIR / data/pack).`;

const { values, positionals } = parseArgs({
  allowPositionals: true,
  options: {
    remote: { type: 'string' },
    token: { type: 'string' },
    neoforge: { type: 'string' },
    mc: { type: 'string' },
    server: { type: 'string' },
    'min-ram': { type: 'string' },
    news: { type: 'string' },
    'news-file': { type: 'string' },
    'pack-version': { type: 'string' },
    help: { type: 'boolean', short: 'h' },
  },
});

if (values.help) {
  console.log(HELP);
  process.exit(0);
}

try {
  const meta = {};
  if (values.neoforge) meta.neoforgeVersion = values.neoforge;
  if (values.mc) meta.mcVersion = values.mc;
  if (values.server) meta.serverAddress = values.server;
  if (values['min-ram']) meta.minRamMb = Number(values['min-ram']);
  if (values['pack-version']) meta.packVersion = values['pack-version'];
  if (values['news-file']) meta.news = (await readFile(values['news-file'], 'utf8')).trim();
  else if (values.news !== undefined) meta.news = values.news;

  const src = positionals[0];
  let result;
  if (values.remote) {
    const token = values.token || process.env.ADMIN_TOKEN;
    if (!token) throw new Error('Informe --token ou defina ADMIN_TOKEN');
    if (!src) {
      const r = await fetch(`${values.remote.replace(/\/+$/, '')}/api/admin/publish`, {
        method: 'POST',
        headers: { 'x-admin-token': token, 'content-type': 'application/json' },
        body: JSON.stringify(meta),
      });
      if (!r.ok) throw new Error(`HTTP ${r.status}: ${await r.text()}`);
      result = await r.json();
    } else result = await publishRemote(src, values.remote, token, meta);
  } else {
    const store = new PackStore(loadConfig().packDir);
    await store.init();
    if (!src) {
      const r = await store.publish(meta);
      result = { packVersion: r.manifest.packVersion, files: r.manifest.files.length };
    } else result = await publishLocal(src, store, meta);
  }
  const version = result.packVersion ?? result.manifest?.packVersion;
  console.log(`\nPublicado! Pack versão ${version} — ${result.files ?? result.manifest?.files.length} arquivos` +
    (result.uploaded !== undefined ? ` (${result.uploaded} enviados, ${result.removed} removidos)` : ''));
  const skipped = [...(result.skippedSource || []), ...(result.skipped || [])];
  if (skipped.length) console.log(`Ignorados (fora da whitelist/nome inválido): ${[...new Set(skipped)].join(', ')}`);
} catch (e) {
  console.error(`\nErro: ${e.message}`);
  process.exit(1);
}
