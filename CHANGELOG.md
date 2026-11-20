# Changelog

Formato baseado em [Keep a Changelog](https://keepachangelog.com/pt-BR/1.1.0/) e versionamento [SemVer](https://semver.org/lang/pt-BR/).

## [1.0.0] - 2026-11-20

### Adicionado
- **Launcher para Windows** (Tauri 2 + Rust) com 1 clique: instala o Java 21 (Temurin), o Minecraft 1.21.1, o NeoForge e o modpack, e abre o jogo já conectado no servidor (`--quickPlayMultiplayer`).
- **Contas offline**: só o nome de jogador, com o mesmo UUID que o servidor gera em `online-mode=false`.
- **Sincronização do modpack** por sha256: baixa só o que mudou, remove de `mods/` o que saiu do pack e retoma downloads interrompidos.
- **Instalação do NeoForge** pelo installer oficial em modo headless; o comando de execução é montado pelo próprio launcher.
- **Modo offline**: se o servidor de atualizações cair, o launcher abre com a última versão instalada.
- **Console** com o log do jogo, detecção de crash com resumo em português e botão para copiar o log.
- **Status do servidor** (ping do protocolo do Minecraft) e **notícias** em Markdown vindas da API.
- **Configurações**: RAM com padrão inteligente, resolução, entrar direto no servidor, pasta do jogo, argumentos da Java, reparar instalação e limpar cache.
- **Auto-update** do launcher pelo updater assinado do Tauri.
- **API** (Node + Fastify, sem banco) que distribui o pack, as notícias e as versões do launcher, com `publish-pack` e `publish-launcher`.
- Visual próprio "deepslate + glowstone": janela sem moldura, fontes embutidas, botão Jogar em bloco, terreno em pixel-art e ícone novo.
- Build do `.exe` e do instalador nas **Releases** do GitHub.

[1.0.0]: https://github.com/stumdev/stlauncher/releases/tag/v1.0.0
