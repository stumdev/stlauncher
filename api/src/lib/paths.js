// Validação de caminhos relativos do pack. O MESMO conjunto de regras existe no launcher (Rust):
// um manifest nunca pode escrever fora da pasta do jogo.

/** Pastas de topo permitidas no pack. */
export const ALLOWED_ROOTS = ['mods', 'config', 'resourcepacks', 'shaderpacks'];
/** Arquivos soltos permitidos na raiz do pack. */
export const ALLOWED_FILES = ['servers.dat'];

const BAD_CHARS = /[<>:"|?*\\\u0000-\u001f]/;
const RESERVED = /^(con|prn|aux|nul|com[1-9]|lpt[1-9])(\..*)?$/i;

/**
 * Valida um caminho relativo usando "/" como separador. Retorna o próprio caminho
 * ou lança Error com mensagem em português.
 */
export function validateRelPath(p) {
  if (typeof p !== 'string' || p.length === 0) throw new Error('caminho vazio');
  if (p.length > 240) throw new Error(`caminho muito longo: ${p}`);
  if (p.startsWith('/')) throw new Error(`caminho absoluto não permitido: ${p}`);
  const parts = p.split('/');
  for (const part of parts) {
    if (part === '' || part === '.' || part === '..') throw new Error(`caminho inválido: ${p}`);
    if (BAD_CHARS.test(part)) throw new Error(`caractere inválido no caminho: ${p}`);
    if (/[. ]$/.test(part)) throw new Error(`nome não pode terminar com ponto/espaço: ${p}`);
    if (RESERVED.test(part)) throw new Error(`nome reservado do Windows: ${p}`);
  }
  if (parts.length === 1) {
    if (!ALLOWED_FILES.includes(p)) throw new Error(`arquivo de raiz não permitido: ${p}`);
  } else if (!ALLOWED_ROOTS.includes(parts[0])) {
    throw new Error(`pasta não permitida no pack: ${parts[0]}/ (use ${ALLOWED_ROOTS.join(', ')})`);
  }
  return p;
}

/** Codifica cada segmento para uso em URL. */
export function encodePathForUrl(p) {
  return p.split('/').map(encodeURIComponent).join('/');
}
