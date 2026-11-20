// Aplicação Fastify: rotas públicas (manifest, versão do launcher, arquivos) e rotas admin.
import { createHash, timingSafeEqual } from 'node:crypto';
import { createWriteStream } from 'node:fs';
import { mkdir, readFile, rename, stat, unlink } from 'node:fs/promises';
import path from 'node:path';
import { pipeline } from 'node:stream/promises';
import Fastify from 'fastify';
import rateLimit from '@fastify/rate-limit';
import fastifyStatic from '@fastify/static';
import { MIN_TOKEN_LENGTH } from './config.js';
import { PackStore, writeJsonAtomic } from './lib/store.js';

const SEMVER = /^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/;
const SAFE_NAME = /^[A-Za-z0-9._-]{1,120}$/;

/**
 * Caminho relativo a partir da URL CRUA, decodificando tudo (inclusive %2B, %23, %26...).
 * Lança erro se a codificação for inválida. A validação de segurança é feita depois (validateRelPath).
 */
export function relFromUrl(url, prefix) {
  const raw = url.split('?')[0];
  if (!raw.startsWith(prefix)) throw new Error('prefixo inválido');
  return raw
    .slice(prefix.length)
    .split('/')
    .map((s) => decodeURIComponent(s))
    .join('/');
}

const sha256 = (s) => createHash('sha256').update(s).digest();
function tokenMatches(given, expected) {
  if (typeof given !== 'string' || !expected) return false;
  return timingSafeEqual(sha256(given), sha256(expected)); // tempo constante
}

export async function createApp(config, { logger = { level: 'info' } } = {}) {
  const app = Fastify({ logger, trustProxy: config.trustProxy });
  const store = new PackStore(config.packDir);
  await store.init();
  await mkdir(path.join(config.launcherDir, 'files'), { recursive: true });
  const launcherJson = path.join(config.launcherDir, 'launcher.json');

  app.setErrorHandler((err, req, reply) => {
    const code = err.statusCode && err.statusCode < 600 ? err.statusCode : 500;
    if (code >= 500) req.log.error(err);
    reply.code(code).send({ error: code >= 500 ? 'Erro interno do servidor' : err.message });
  });
  app.setNotFoundHandler((req, reply) => reply.code(404).send({ error: 'Não encontrado' }));

  // Arquivos do pack: sem rate limit (o launcher baixa dezenas de arquivos em paralelo).
  // `reply.sendFile` usa `send`, que implementa Range/If-Range/ETag (retomada de download).
  // A rota é nossa (serve: false) porque o roteador NÃO decodifica caracteres reservados como
  // %2B (+), %23 (#), %26 (&): mods como "puzzleslib-...+neoforge.jar" davam 404.
  await app.register(fastifyStatic, { root: store.filesDir, serve: false, cacheControl: true, maxAge: 0 });
  app.get('/files/*', (req, reply) => {
    let rel;
    try {
      rel = relFromUrl(req.url, '/files/');
      store.absPath(rel); // valida whitelist + impede sair da pasta
    } catch {
      return reply.code(404).send({ error: 'Não encontrado' });
    }
    return reply.sendFile(rel, store.filesDir, { dotfiles: 'deny', index: false });
  });
  await app.register(fastifyStatic, {
    root: path.join(config.launcherDir, 'files'),
    prefix: '/launcher-files/',
    index: false,
    dotfiles: 'deny',
    decorateReply: false,
  });

  const baseUrl = (req) => config.publicUrl || `${req.protocol}://${req.host}`;

  // ---------- API pública (com rate limit) ----------
  await app.register(async (pub) => {
    await pub.register(rateLimit, { max: config.rateLimitMax, timeWindow: '1 minute' });

    pub.get('/api/health', async () => ({ ok: true }));

    pub.get('/api/pack/manifest', async (req, reply) => {
      const m = await store.readManifest();
      if (!m) return reply.code(404).send({ error: 'Nenhum pack publicado ainda' });
      reply.header('Cache-Control', 'no-cache');
      return m;
    });

    // Formato compatível com o updater do Tauri (version + platforms) e com o pedido original (version, url, notes).
    pub.get('/api/launcher/version', async (req, reply) => {
      let info;
      try {
        info = JSON.parse(await readFile(launcherJson, 'utf8'));
      } catch (e) {
        if (e.code !== 'ENOENT') throw e;
        return reply.code(204).send(); // nada publicado: "sem atualização"
      }
      const url = `${baseUrl(req)}/launcher-files/${encodeURIComponent(info.file)}`;
      reply.header('Cache-Control', 'no-cache');
      return {
        version: info.version,
        notes: info.notes || '',
        pub_date: info.pub_date,
        url,
        platforms: { 'windows-x86_64': { signature: info.signature, url } },
      };
    });
  });

  // ---------- Admin (token no header) ----------
  await app.register(async (admin) => {
    const enabled = config.adminToken.length >= MIN_TOKEN_LENGTH;
    await admin.register(rateLimit, { max: 2000, timeWindow: '1 minute' });
    // Upload bruto em streaming (arquivos grandes não passam pela memória).
    admin.addContentTypeParser('application/octet-stream', (req, payload, done) => done(null, payload));

    admin.addHook('onRequest', async (req, reply) => {
      if (!enabled) return reply.code(503).send({ error: `Admin desativado: defina ADMIN_TOKEN com ao menos ${MIN_TOKEN_LENGTH} caracteres` });
      const auth = req.headers.authorization;
      const given = req.headers['x-admin-token'] || (auth && auth.startsWith('Bearer ') ? auth.slice(7) : '');
      if (!tokenMatches(given, config.adminToken)) {
        req.log.warn({ ip: req.ip }, 'admin: token inválido');
        return reply.code(401).send({ error: 'Token inválido' });
      }
    });

    // Upload de 1 arquivo do pack. Header opcional x-sha256 é conferido durante o stream.
    admin.put('/api/admin/files/*', async (req, reply) => {
      let rel;
      try {
        rel = relFromUrl(req.url, '/api/admin/files/');
      } catch {
        return reply.code(400).send({ error: 'caminho mal codificado' });
      }
      let dest;
      try {
        dest = store.absPath(rel);
      } catch (e) {
        return reply.code(400).send({ error: e.message });
      }
      if (!req.body || typeof req.body.pipe !== 'function')
        return reply.code(415).send({ error: 'Envie o corpo como application/octet-stream' });
      await mkdir(path.dirname(dest), { recursive: true });
      const tmp = `${dest}.${process.pid}.upload`;
      const hash = createHash('sha256');
      let size = 0;
      try {
        await pipeline(
          req.body,
          async function* (src) {
            for await (const c of src) {
              hash.update(c);
              size += c.length;
              yield c;
            }
          },
          createWriteStream(tmp),
        );
        const got = hash.digest('hex');
        const want = req.headers['x-sha256'];
        if (want && String(want).toLowerCase() !== got) {
          await unlink(tmp);
          return reply.code(422).send({ error: 'sha256 não confere (upload corrompido)' });
        }
        await rename(tmp, dest);
        return { path: rel, size, sha256: got };
      } catch (e) {
        await unlink(tmp).catch(() => {});
        throw e;
      }
    });

    admin.delete('/api/admin/files/*', async (req, reply) => {
      try {
        await store.removeFile(relFromUrl(req.url, '/api/admin/files/'));
      } catch (e) {
        if (e.code === 'ENOENT') return reply.code(404).send({ error: 'Arquivo não existe' });
        return reply.code(400).send({ error: e.message });
      }
      return { ok: true };
    });

    // Reescaneia files/ e gera novo manifest. Corpo opcional: {mcVersion, neoforgeVersion, serverAddress, minRamMb, news, packVersion}
    admin.post('/api/admin/publish', async (req) => {
      const { manifest, skipped, changed } = await store.publish(req.body && typeof req.body === 'object' ? req.body : {});
      return { packVersion: manifest.packVersion, files: manifest.files.length, changed, skipped };
    });

    // Publicação de nova versão do launcher (instalador já enviado via PUT /api/admin/launcher-files/:name).
    admin.put('/api/admin/launcher-files/:name', async (req, reply) => {
      const { name } = req.params;
      if (!SAFE_NAME.test(name)) return reply.code(400).send({ error: 'nome de arquivo inválido' });
      if (!req.body || typeof req.body.pipe !== 'function')
        return reply.code(415).send({ error: 'Envie o corpo como application/octet-stream' });
      const dest = path.join(config.launcherDir, 'files', name);
      const tmp = `${dest}.${process.pid}.upload`;
      try {
        await pipeline(req.body, createWriteStream(tmp));
        await rename(tmp, dest);
      } catch (e) {
        await unlink(tmp).catch(() => {});
        throw e;
      }
      return { ok: true };
    });

    admin.post('/api/admin/launcher', async (req, reply) => {
      const { version, notes = '', file, signature } = req.body || {};
      if (!SEMVER.test(version || '')) return reply.code(400).send({ error: 'version inválida (ex: 1.0.1)' });
      if (!SAFE_NAME.test(file || '')) return reply.code(400).send({ error: 'file inválido' });
      if (typeof signature !== 'string' || signature.length < 20)
        return reply.code(400).send({ error: 'signature (conteúdo do .sig) obrigatória' });
      await stat(path.join(config.launcherDir, 'files', file)).catch(() => {
        throw Object.assign(new Error('arquivo do instalador não encontrado no servidor'), { statusCode: 400 });
      });
      await writeJsonAtomic(launcherJson, { version, notes: String(notes), file, signature, pub_date: new Date().toISOString() });
      return { ok: true, version };
    });
  });

  return app;
}
