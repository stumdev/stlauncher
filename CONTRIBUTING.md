# Contribuindo com o STLauncher

Obrigado por querer ajudar! Qualquer contribuição é bem-vinda: bugs, ideias, documentação e código.

## Preparando o ambiente

Você vai precisar de **Node.js 22+**, **Rust** (stable, alvo MSVC) e do **Visual Studio Build Tools** com a carga "Desenvolvimento para desktop com C++". O WebView2 já vem no Windows 10/11.

```bash
git clone https://github.com/stumdev/stlauncher.git
cd stlauncher

# API (em um terminal)
cd api
npm ci
cp .env.example .env            # coloque um ADMIN_TOKEN de 16+ caracteres
npm start                       # http://localhost:8080

# launcher (em outro terminal)
cd launcher
npm ci
npm run tauri dev               # abre o launcher em modo desenvolvimento
```

Para testar contra a API local, troque `apiUrl` em `launcher/launcher.config.json` por `http://127.0.0.1:8080` (sem commitar essa mudança).

## Fluxo

1. Faça um fork e crie uma branch: `git checkout -b minha-mudanca`.
2. Faça a mudança, com testes quando fizer sentido.
3. Rode as verificações:
   - API: `npm test` (em `api/`);
   - launcher: `cargo test --lib` (em `launcher/src-tauri/`), `npm run build` e `node --test src/markdown.test.ts` (em `launcher/`).
4. Abra um Pull Request explicando o que mudou e por quê.

Use [Conventional Commits](https://www.conventionalcommits.org/pt-br/) nas mensagens (`feat:`, `fix:`, `docs:`, `chore:`).

## Boas práticas

- Textos do launcher e documentação em português do Brasil, escritos para quem é leigo.
- Toda mensagem de erro deve dizer **o que aconteceu e o que fazer**.
- Nada de travar a interface: download e disco sempre assíncronos.
- Caminhos vindos do manifest passam sempre pela whitelist (`api/src/lib/paths.js` e `launcher/src-tauri/src/pack.rs` precisam continuar iguais).
- **Nunca commite** `.env`, `ADMIN_TOKEN`, a chave privada do updater (`*.key`) nem a pasta `data/` da API.
