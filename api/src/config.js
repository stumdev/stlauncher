// Configuração da API, toda vinda de variáveis de ambiente (.env opcional).
import { existsSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

// Carrega api/.env se existir (Node >= 20.12). Variáveis já definidas no ambiente têm prioridade.
const envFile = path.join(root, '.env');
if (existsSync(envFile)) process.loadEnvFile(envFile);

export function loadConfig(env = process.env) {
  const num = (v, d) => {
    const n = Number(v);
    return Number.isFinite(n) && n > 0 ? n : d;
  };
  const dataDir = path.resolve(root, env.DATA_DIR || './data');
  return {
    host: env.HOST || '0.0.0.0',
    port: num(env.PORT, 8080),
    dataDir,
    // Pasta do pack (manifest.json + files/). Padrão: <DATA_DIR>/pack
    packDir: path.resolve(root, env.PACK_DIR || path.join(dataDir, 'pack')),
    // Pasta dos instaladores do launcher. Padrão: <DATA_DIR>/launcher
    launcherDir: path.resolve(root, env.LAUNCHER_DIR || path.join(dataDir, 'launcher')),
    adminToken: env.ADMIN_TOKEN || '',
    // URL pública (ex: https://api.meusite.com). Se vazio, deduz do header Host.
    publicUrl: (env.PUBLIC_URL || '').replace(/\/+$/, ''),
    rateLimitMax: num(env.RATE_LIMIT_MAX, 120),
    trustProxy: env.TRUST_PROXY === 'true',
  };
}

/** Token de admin precisa ter tamanho mínimo, senão o admin fica desativado. */
export const MIN_TOKEN_LENGTH = 16;
