# Política de Segurança

O STLauncher baixa e executa programas (Java, Minecraft, mods) no computador dos jogadores, então levamos segurança a sério.

## Reportando uma vulnerabilidade

Se encontrou uma falha de segurança, **não abra uma issue pública**. Use o recurso [Report a vulnerability](https://github.com/stumdev/stlauncher/security/advisories/new) do GitHub e descreva o problema com o máximo de detalhes possível.

## O que já protege os jogadores

- **Atualizações do launcher assinadas**: o updater só instala pacotes assinados com a chave do mantenedor, mesmo que o servidor seja comprometido.
- **Todo arquivo é conferido por hash**: SHA-1 para os arquivos da Mojang, SHA-256 para o Java (Adoptium) e para o modpack.
- **Caminhos do manifest na whitelist**: só `mods/`, `config/`, `resourcepacks/`, `shaderpacks/` e `servers.dat`; nada de `..`, caminhos absolutos ou nomes reservados do Windows.
- **Zip do Java protegido contra zip-slip** e notícias em Markdown renderizadas com escape total de HTML.
- **Admin da API** só com token de 16+ caracteres, comparado em tempo constante; sem token configurado, as rotas de admin ficam desligadas.

## Boas práticas ao rodar a API

- Nunca versione `.env`, `data/` nem a chave privada do updater (o `.gitignore` já cobre).
- Exponha a API com HTTPS (Cloudflare Tunnel, Caddy ou nginx) e use `TRUST_PROXY=true` atrás do proxy.
- Faça backup da chave privada do updater (`~/.tauri/stlauncher.key`): sem ela, os launchers instalados deixam de receber atualizações.
