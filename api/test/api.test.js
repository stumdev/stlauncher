import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, readFile, rm, writeFile, stat } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { after, before, describe, test } from 'node:test';
import { createApp } from '../src/app.js';
import { loadConfig } from '../src/config.js';
import { validateRelPath } from '../src/lib/paths.js';
import { diffFiles, publishLocal, publishRemote } from '../src/lib/publish.js';
import { PackStore, scanFiles } from '../src/lib/store.js';

const TOKEN = 'token-de-teste-bem-longo-123';
const sha = (b) => createHash('sha256').update(b).digest('hex');
const META = { neoforgeVersion: '21.1.172', serverAddress: 'play.exemplo.com:25565', minRamMb: 4096, news: '**Evento** sexta 20h' };

describe('validateRelPath', () => {
  test('aceita caminhos válidos (com acento e espaço)', () => {
    for (const p of ['mods/a.jar', 'config/sub/x.toml', 'mods/Mód Legal 1.0.jar', 'servers.dat', 'shaderpacks/s.zip'])
      assert.equal(validateRelPath(p), p);
  });
  test('rejeita traversal, absolutos e coisas do Windows', () => {
    for (const p of ['../x', 'mods/../../x', '/mods/a', 'mods//a', 'mods\\a.jar', 'C:/x', 'mods/a:b', 'mods/CON', 'mods/nul.txt',
      'mods/a.', 'mods/a ', 'logs/x.txt', 'options.txt', 'mods', '', 'mods/./a'])
      assert.throws(() => validateRelPath(p), undefined, p);
  });
});

describe('diffFiles', () => {
  test('calcula upload e remoção', () => {
    const src = [{ path: 'mods/a', sha256: '1' }, { path: 'mods/b', sha256: '2' }, { path: 'mods/c', sha256: '3' }];
    const dest = [{ path: 'mods/a', sha256: '1' }, { path: 'mods/b', sha256: 'X' }, { path: 'mods/old', sha256: '9' }];
    const d = diffFiles(src, dest);
    assert.deepEqual(d.upload.map((f) => f.path), ['mods/b', 'mods/c']);
    assert.deepEqual(d.remove, ['mods/old']);
  });
});

let tmp, src, config, app, base;

async function makeSrc() {
  await mkdir(path.join(src, 'mods'), { recursive: true });
  await mkdir(path.join(src, 'config', 'sub'), { recursive: true });
  await mkdir(path.join(src, 'logs'), { recursive: true });
  await writeFile(path.join(src, 'mods', 'Mód com espaço.jar'), 'conteudo-do-mod-1');
  await writeFile(path.join(src, 'mods', 'b.jar'), 'b'.repeat(5000));
  await writeFile(path.join(src, 'config', 'sub', 'x.toml'), 'a=1');
  await writeFile(path.join(src, 'servers.dat'), 'SD');
  await writeFile(path.join(src, 'logs', 'latest.log'), 'ignorar');
  await writeFile(path.join(src, 'options.txt'), 'ignorar');
}

before(async () => {
  tmp = await mkdtemp(path.join(tmpdir(), 'stl-api-'));
  src = path.join(tmp, 'src');
  config = loadConfig({ DATA_DIR: path.join(tmp, 'data'), ADMIN_TOKEN: TOKEN, RATE_LIMIT_MAX: '1000' });
  await makeSrc();
  app = await createApp(config, { logger: false });
  base = await app.listen({ port: 0, host: '127.0.0.1' });
});
after(async () => {
  await app.close();
  await rm(tmp, { recursive: true, force: true });
});

describe('API', () => {
  test('manifest 404 antes de publicar e launcher 204', async () => {
    assert.equal((await fetch(`${base}/api/pack/manifest`)).status, 404);
    assert.equal((await fetch(`${base}/api/launcher/version`)).status, 204);
  });

  test('admin: sem token 401, token errado 401', async () => {
    assert.equal((await fetch(`${base}/api/admin/publish`, { method: 'POST' })).status, 401);
    assert.equal((await fetch(`${base}/api/admin/publish`, { method: 'POST', headers: { 'x-admin-token': 'errado' } })).status, 401);
  });

  test('publishLocal gera manifest correto e ignora fora da whitelist', async () => {
    const store = new PackStore(config.packDir);
    const r = await publishLocal(src, store, META, () => {});
    assert.equal(r.uploaded, 4);
    assert.ok(r.skippedSource.includes('logs/') && r.skippedSource.includes('options.txt'));
    const m = JSON.parse(await readFile(store.manifestPath, 'utf8'));
    assert.equal(m.neoforgeVersion, '21.1.172');
    assert.equal(m.mcVersion, '1.21.1');
    assert.equal(m.files.length, 4);
    const f = m.files.find((x) => x.path === 'mods/b.jar');
    assert.equal(f.sha256, sha('b'.repeat(5000)));
    assert.equal(f.size, 5000);
    assert.equal(f.url, '/files/mods/b.jar');
    assert.equal(m.files.find((x) => x.path.startsWith('mods/M')).url, '/files/mods/M%C3%B3d%20com%20espa%C3%A7o.jar');
  });

  test('GET manifest público + sem CORS', async () => {
    const res = await fetch(`${base}/api/pack/manifest`);
    assert.equal(res.status, 200);
    assert.equal(res.headers.get('access-control-allow-origin'), null);
    assert.equal((await res.json()).files.length, 4);
  });

  test('/files serve arquivo (url codificada) e suporta Range', async () => {
    const res = await fetch(`${base}/files/mods/M%C3%B3d%20com%20espa%C3%A7o.jar`);
    assert.equal(res.status, 200);
    assert.equal(await res.text(), 'conteudo-do-mod-1');
    const part = await fetch(`${base}/files/mods/b.jar`, { headers: { range: 'bytes=4990-' } });
    assert.equal(part.status, 206);
    assert.equal(part.headers.get('content-range'), 'bytes 4990-4999/5000');
    assert.equal((await part.text()).length, 10);
  });

  test('nomes com + # & [ ] % espaço e acento: upload, manifest e download (regressão do 404 com %2B)', async () => {
    const names = [
      'puzzleslib-v21.1.66-mc1.21.1+neoforge.jar',
      '[1.21.1] SecurityCraft v1.10.2.1.jar',
      'a#b&c=d;e,f@g$h.jar',
      'cem%por cento ç.jar',
    ];
    for (const n of names) {
      const body = `conteudo de ${n}`;
      const up = await fetch(`${base}/api/admin/files/mods/${encodeURIComponent(n)}`, {
        method: 'PUT',
        headers: { 'x-admin-token': TOKEN, 'content-type': 'application/octet-stream', 'x-sha256': sha(body) },
        body,
      });
      assert.equal(up.status, 200, `upload ${n}`);
    }
    await fetch(`${base}/api/admin/publish`, { method: 'POST', headers: { 'x-admin-token': TOKEN } });
    const m = await (await fetch(`${base}/api/pack/manifest`)).json();
    for (const n of names) {
      const f = m.files.find((x) => x.path === `mods/${n}`);
      assert.ok(f, `no manifest: ${n}`);
      // baixa exatamente pela URL do manifest, como o launcher faz
      const res = await fetch(`${base}${f.url}`);
      assert.equal(res.status, 200, `download ${n} via ${f.url}`);
      assert.equal(await res.text(), `conteudo de ${n}`);
      const part = await fetch(`${base}${f.url}`, { headers: { range: 'bytes=0-3' } });
      assert.equal(part.status, 206, `range ${n}`);
      // e remove via admin
      const del = await fetch(`${base}/api/admin/files/mods/${encodeURIComponent(n)}`, { method: 'DELETE', headers: { 'x-admin-token': TOKEN } });
      assert.equal(del.status, 200, `delete ${n}`);
    }
    await fetch(`${base}/api/admin/publish`, { method: 'POST', headers: { 'x-admin-token': TOKEN } });
    // codificação inválida: o Fastify recusa (400) antes da rota; o importante é nunca servir nada
    assert.ok([400, 404].includes((await fetch(`${base}/files/mods/%ZZ`)).status));
  });

  test('/files bloqueia traversal e arquivos internos', async () => {
    for (const u of ['/files/../meta.json', '/files/%2e%2e/meta.json', '/files/..%2fmanifest.json'])
      assert.notEqual((await fetch(`${base}${u}`)).status, 200, u);
  });

  test('upload remoto: x-sha256 errado é rejeitado e não deixa arquivo', async () => {
    const r = await fetch(`${base}/api/admin/files/mods/novo.jar`, {
      method: 'PUT',
      headers: { 'x-admin-token': TOKEN, 'content-type': 'application/octet-stream', 'x-sha256': 'a'.repeat(64) },
      body: 'dados',
    });
    assert.equal(r.status, 422);
    await assert.rejects(stat(path.join(config.packDir, 'files', 'mods', 'novo.jar')));
  });

  test('upload rejeita caminho inválido', async () => {
    const r = await fetch(`${base}/api/admin/files/logs/x.txt`, {
      method: 'PUT', headers: { 'x-admin-token': TOKEN, 'content-type': 'application/octet-stream' }, body: 'x',
    });
    assert.equal(r.status, 400);
  });

  test('publishRemote envia só o que mudou, remove o que sumiu', async () => {
    // altera a origem: muda b.jar, remove x.toml, adiciona c.jar
    await writeFile(path.join(src, 'mods', 'b.jar'), 'novo-b');
    await rm(path.join(src, 'config', 'sub', 'x.toml'));
    await writeFile(path.join(src, 'mods', 'c.jar'), 'ccc');
    const r = await publishRemote(src, base, TOKEN, { news: 'Novidade!' }, () => {});
    assert.equal(r.uploaded, 2);
    assert.equal(r.removed, 1);
    const m = await (await fetch(`${base}/api/pack/manifest`)).json();
    assert.deepEqual(m.files.map((f) => f.path).sort(), ['mods/M\u00f3d com espa\u00e7o.jar', 'mods/b.jar', 'mods/c.jar', 'servers.dat']);
    assert.equal(m.news, 'Novidade!');
    assert.equal(m.serverAddress, 'play.exemplo.com:25565'); // meta persistida
    const b = await fetch(`${base}/files/mods/b.jar`);
    assert.equal(await b.text(), 'novo-b');
    // arquivo removido não é mais servido e a pasta vazia sumiu
    assert.equal((await fetch(`${base}/files/config/sub/x.toml`)).status, 404);
  });

  test('republicar sem mudar arquivos mantém packVersion; meta inválida dá 400', async () => {
    const v1 = (await (await fetch(`${base}/api/pack/manifest`)).json()).packVersion;
    const r = await fetch(`${base}/api/admin/publish`, {
      method: 'POST', headers: { 'x-admin-token': TOKEN, 'content-type': 'application/json' }, body: JSON.stringify({ news: 'outra' }),
    });
    assert.equal(r.status, 200);
    assert.equal((await r.json()).changed, false);
    assert.equal((await (await fetch(`${base}/api/pack/manifest`)).json()).packVersion, v1);
    const bad = await fetch(`${base}/api/admin/publish`, {
      method: 'POST', headers: { 'x-admin-token': TOKEN, 'content-type': 'application/json' }, body: JSON.stringify({ minRamMb: 5 }),
    });
    assert.equal(bad.status, 400);
  });

  test('launcher: publica e expõe formato do updater do Tauri', async () => {
    const h = { 'x-admin-token': TOKEN };
    const up = await fetch(`${base}/api/admin/launcher-files/Setup_1.0.1.exe`, {
      method: 'PUT', headers: { ...h, 'content-type': 'application/octet-stream' }, body: 'EXE',
    });
    assert.equal(up.status, 200);
    const pub = await fetch(`${base}/api/admin/launcher`, {
      method: 'POST', headers: { ...h, 'content-type': 'application/json' },
      body: JSON.stringify({ version: '1.0.1', notes: 'Correções', file: 'Setup_1.0.1.exe', signature: 'assinatura-de-teste-xxxxxxxxxxxxxxxx' }),
    });
    assert.equal(pub.status, 200);
    const v = await (await fetch(`${base}/api/launcher/version`)).json();
    assert.equal(v.version, '1.0.1');
    assert.equal(v.notes, 'Correções');
    assert.equal(v.url, `${base}/launcher-files/Setup_1.0.1.exe`);
    assert.equal(v.platforms['windows-x86_64'].url, v.url);
    assert.equal(await (await fetch(v.url)).text(), 'EXE');
  });

});

test('rate limit: excedido retorna 429 na API pública', async () => {
  const c = loadConfig({ DATA_DIR: path.join(tmp, 'data3'), RATE_LIMIT_MAX: '3' });
  const a = await createApp(c, { logger: false });
  const codes = [];
  for (let i = 0; i < 5; i++) codes.push((await a.inject({ url: '/api/health' })).statusCode);
  assert.deepEqual(codes, [200, 200, 200, 429, 429]);
  await a.close();
});

test('admin desativado sem token configurado', async () => {
  const c = loadConfig({ DATA_DIR: path.join(tmp, 'data2') });
  const a = await createApp(c, { logger: false });
  const r = await a.inject({ method: 'POST', url: '/api/admin/publish', headers: { 'x-admin-token': 'qualquer' } });
  assert.equal(r.statusCode, 503);
  await a.close();
});

test('scanFiles lista arquivos da whitelist', async () => {
  const d = path.join(tmp, 'case');
  await mkdir(path.join(d, 'mods'), { recursive: true });
  await writeFile(path.join(d, 'mods', 'A.jar'), '1');
  const { files } = await scanFiles(d);
  assert.equal(files.length, 1);
});
