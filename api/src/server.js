import { createApp } from './app.js';
import { loadConfig, MIN_TOKEN_LENGTH } from './config.js';

const config = loadConfig();
const app = await createApp(config);

if (config.adminToken.length < MIN_TOKEN_LENGTH)
  app.log.warn(`ADMIN_TOKEN ausente/curto (< ${MIN_TOKEN_LENGTH}): rotas /api/admin desativadas.`);

try {
  await app.listen({ host: config.host, port: config.port });
} catch (e) {
  app.log.error(e, e.code === 'EADDRINUSE' ? `Porta ${config.port} já está em uso` : 'Falha ao iniciar');
  process.exit(1);
}
for (const sig of ['SIGINT', 'SIGTERM']) process.on(sig, () => app.close().then(() => process.exit(0)));
