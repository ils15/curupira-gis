# Bench de decisão — render de mapa (Fase 3)

> Contrato: PLAN.md»Fase 3 — "bench documentado com gatilho objetivo p/ Fase
> 3b condicional". Critério de escape: **p95 > 33 ms (< 30 fps) → PROPOSTA de
> Fase 3b (Tauri+MapLibre)**. Harness: `crates/ta-gui/benches/map_render.rs`
> (`cargo bench -p ta-gui`), dados gerados por
> `scripts/gen_bench_fixtures.py` (seed 42, determinístico — DISCOVERY.md»Bench
> Dataset Spec).

## Resultado executado (2026-10-16, host do projeto)

Ambiente: Ubuntu 24.04, rustc 1.99.1 stable, perfil `bench` (opt-level 3),
**headless** (`egui::Context::run_pass` — o MESMO caminho de código do app:
`MapViewEgui::render` + tesselação CPU do egui). Viewport 1280×800 px,
feathering on, 5 frames de warm-up + 120 medidos por cenário.

Dados (`testdata/bench/`, gerado, não commitado; manifest versionado):
`bench_polys.shp` 10 000 polígonos irregulares EPSG:4674 ·
`bench_points.csv` 50 000 pontos CSV-XY EPSG:4674 ·
`bench_raster.tif` 1024², 4 bandas UInt16, tiled 256, LZW ·
`bench_raster_stress.tif` 8192², 1 banda (gerado com `--stress`).

| cenário | mean (ms) | p95 (ms) | max (ms) | bytes de dados (est.) |
|---|---|---|---|---|
| 10k_poligonos | 2.06 | 3.14 | 3.29 | ~3.0 MiB |
| 50k_pontos | 2.53 | 2.59 | 12.49 | ~4.5 MiB |
| ambos_mais_raster_1024 | 6.96 | 8.22 | 10.30 | ~7.5 MiB |
| raster_stress_8192 (só CPU headless) | 0.01 | 0.01 | 0.03 | 16 MiB de textura |

Variação entre execuções observada: p95 ±15% (host compartilhado); pior p95
registrado nas execuções desta fase: **9.47 ms** (ambos+raster) — ainda 3.5×
abaixo do orçamento.

Carga dos dados (leitura SHP/CSV + normalização 4674): **237 ms** para
10k + 50k (fora do loop de frames). RSS do processo: Δ ~62 MiB com os três
datasets em memória.

## Decisão: **Fase 3b NÃO é disparada**

- p95 pior caso vetorial+րaster = **8.22 ms** — 4× abaixo do orçamento de
  33 ms (30 fps). Sobrando margem até na máquina de CI mais lenta (~4×).
- O raster 8192² não pressionou o caminho CPU: a banda é decimada UMA vez no
  carregamento (GDAL, 8192→2048, `raster_view::load_band`) e o frame só
  emite a textura escalada (O(viewport)).

### Otimizações já aplicadas nesta fase (medidas antes/depois)

1. **LOD pontual** (`map_view::point_stride`, orçamento 20 000 pontos):
   50k pontos caíram de **143.5 ms mean / 158.2 p95** (sem LOD, círculo duplo)
   para **2.5 ms / 2.6 ms** — stride 3 + disco simples em vez de
   disco+contorno. Este dado valida a política documentada no módulo.
2. **LOD de anéis** (`viewport::lod_stride`): anéis subpixel são pulados e
   anéis pequenos são decimados — parte do porquê 10k polígonos custam ~2 ms.
3. **Culling por bbox pré-computado** (`set_layers` cacheia bbox por feature;
   `intersects_viewport` descarta fora da tela antes de qualquer tesselação).

### Limitações honestas do harness

- **GPU não é medida.** Headless mede CPU (tesselação egui). O custo de
  preenchimento de textura grande (ex.: raster 8192² esticado na tela) é
  rasterização de GPU e não aparece aqui (`raster_stress_8192` = 0.01 ms é
  só o mesh de 2 triângulos). O gatilho 3b por raster grande precisa do
  smoke manual abaixo em máquina com GPU.
- Círculos com contorno (pontos densos em zoom-in com menos de 20k
  visíveis) voltam ao custo do item 1 sem LOD — orçamento ainda folgado
  (20k × 2 shapes ≈ ~60 ms p95 medidos antes do LOD; monitorar se o
  orçamento de pontos subir).
- Uma única medida por cenário por execução; variância observada entre
  execuções ~±15% no max (barulho de host compartilhado).

## Checklist de smoke manual (visual/GPU — impossível headless)

Este host não tem X11/Wayland (`DISPLAY` vazio, sem xvfb). O que ficou
coberto por testes headless (`cargo test`): transformações tela↔mundo, LOD,
hit-test, render produzindo shapes/tesselação, shell completo sem panic,
import local end-to-end (thread + canal + mapa), i18n, **v0.2: bem-vindo
(personas) renderizando, flags por persona, overlays de grade/barra de
escala, preset de escala, persistência do workbench (caminho injetado)**.
O que precisa de olhos humanos em máquina com display:

### v0.2 — paridade de UI (novo)

- [ ] 1ª abertura mostra o modal "Bem-vindo" central (logo tipográfico + 4 cards com ícones desenhados + "Ajuda" + checkbox "Exibir esta janela na abertura"); clicar num card aplica a persona e fecha.
- [ ] Com o checkbox desmarcado, reiniciar o app NÃO reabre o bem-vindo; marcado, reabre (`~/.config/curupira/workbench.json`).
- [ ] Menu Área de Trabalho → trocar persona muda os menus visíveis: Operador (sem Administração/Gerenciamento/Processamento), Administrador (sem Editar/importar/tabela), Assistente (extra: wizard abre com 4 passos).
- [ ] O canvas central NUNCA desaparece em nenhuma persona.
- [ ] Menu Arquivo → Exportar camada ativa gera GeoJSON válido (conferir num leitor); importar via menu/trilho segue funcionando.
- [ ] Barra de status: "Linhas selecionadas" muda ao clicar numa feição; chip SRS verde "EPSG:4674" com projeto, vermelho "SRS desconhecido" sem; dropdown Scale troca a escala (conferir contra o zoom com a roda); engrenagem abre Opções.
- [ ] Mapa → Grade: meridianos/paralelos aparecem com espaçamento "bonito" em todos os zooms; Barra de escala ancla no canto inferior esquerdo com rótulo coerente (m/km).
- [ ] Árvore de camadas: ícone por geometria, botão direito abre contexto (zoom para camada, renomear [com banco], remover, propriedades com CRS/contagem/origem).
- [ ] Processamento (com banco+projeto): Buffer/Interseção/União criam camada derivada visível; interseção sem sobreposição mostra erro legível e NÃO deixa camada órfã.
- [ ] Console SQL (com banco): SELECT simples lista linhas; `DROP TABLE` é recusado com mensagem; resultado limitado a 200 linhas.
- [ ] Toolbars: grupos separados por divisórias, todos os botões com tooltip i18n; dark e light mantêm contraste AA.

### v0.2.1 — navegador de arquivos, recentes, drag & drop (novo)

- [ ] Menu Arquivo → "Navegador de arquivos…", botão 🗂 na toolbar, atalho Ctrl+Shift+O e o botão dentro do diálogo de importar abrem o MESMO modal; Esc fecha nos quatro caminhos.
- [ ] Breadcrumb clicável navega; ↑ sobe; Home/Projeto habilitam quando aplicável; "Dados de teste" só aparece com `testdata/` presente (no dev aparece; no instalador, skip silencioso).
- [ ] Colar caminho inválido + Enter mostra erro inline vermelho; colar um ARQUIVO seleciona-o no diretório-pai; duplo clique entra em pasta / importa arquivo suportado (shp/geojson/gpkg/fgb/tif/csv).
- [ ] Filtro case-insensitive na pasta corrente; com "Buscar em subpastas" ligado aparece spinner "Buscando…" com botão Cancelar; resultados entram na listagem e F3 pula entre eles; Enter no campo de caminho navega; setas movem a seleção (com campo de texto focado, setas pertencem ao texto); Ctrl+F foca o filtro.
- [ ] Painel de detalhes: nome, tamanho (vírgula em pt-BR, ponto em en), última modificação e formato detectado (Shapefile/GeoJSON/… via magic bytes do `ta-gis::detect`).
- [ ] "Importar" só habilita com arquivo suportado selecionado; importar pelo navegador registra o arquivo (+ pasta) em "Abrir recente ▸" e na seção Recentes do navegador.
- [ ] "Abrir recente ▸": clicar num existente importa direto; clicar num que sumiu do disco remove-o da lista na hora (persistido).
- [ ] Arrastar um .geojson do SO para a janela abre o modal de importação pré-preenchido; arrastar vários arquivos mostra "+N na fila" e importa em sequência; cancelar o diálogo descarta a fila; arrastar .txt mostra erro e não abre nada.
- [ ] workbench.json: `recent_files` (máx. 15) e `recent_dirs` (máx. 8) persistem entre sessões; workbench antigo SEM essas chaves abre normal (defaults serde).
- [ ] Windows: navegador idêntico (é 100% egui); breadcrumb mostra `C:\`; til `~` não expande sem HOME (comportamento documentado).

### v0.2.2 — paleta de comandos + caixa de ferramentas (novo)

- [ ] **Ctrl+K em qualquer foco** abre a paleta; digitar "buf" mostra Buffer (Processamento) primeiro; ↑/↓ movem a seleção com realce visível, Enter executa, Esc e clique no backdrop fecham.
- [ ] Paleta casa em pt-BR e en: "imp" acha "Importar…" em pt-BR, "exp" acha "Export…" em en (trocar idioma no menu e repetir).
- [ ] Ações desabilitadas aparecem esmaecidas com motivo no hover: sem banco, Buffer/Console SQL/Desconect mostram "Conecte-se a um banco de dados"; undo sem edições mostra "Nada a desfazer".
- [ ] Cada linha da paleta mostra categoria à direita e atalho quando existe (Ctrl+O/Ctrl+I/Esc); "Paleta de comandos… (Ctrl+K)" aparece no menu Área de Trabalho.
- [ ] Persona filtra a paleta: como Administrador, "import" não traz o comando de importar; como Operador, nada de Gerência de Projeto/Administração.
- [ ] Área de Trabalho → "Caixa de ferramentas" abre a janela flutuante; ela é movível/redimensionável e convive com o canvas (não é modal).
- [ ] Na caixa: busca filtra o catálogo (3 ops PostGIS + Importar + Exportar); Buffer esmaecido sem banco+projeto com motivo no hover; clicar Buffer com banco+projeto abre o diálogo pré-preenchido (`{camada}_buffer`).
- [ ] Toolbars: 📂 mostra "Abrir projeto (Ctrl+O)" e 📥 "Importar camada (Ctrl+I)" no tooltip; ✛/📍/⌗/▭ têm o MESMO visual de hover/ativo (todos `toggle_value`) e o ativo fica realçado.
- [ ] Com projeto aberto, a barra de título mostra "Curupira GIS — {nome do projeto}"; fechando a conexão, volta a "Curupira GIS".
- [ ] Toda ação do menu/toolbar continua funcionando igual após a refactor do registro (regressão: importar, exportar, zoom, overlays, undo/redo, personas, tema, idioma).

### Legado (Fase 3 — manter verde)

- [ ] `cargo run -p ta-gui` abre janela 1280×800; dark/light alternam no menu Área de Trabalho → Tema (contraste AA nos dois).
- [ ] Pan com arraste, zoom com roda ancorada no cursor, botões +/−, Esc cancela modo novo ponto, Ctrl+O/Ctrl+I/Ctrl+Z/Ctrl+Y.
- [ ] Importar `testdata/bench/bench_polys.shp` (local): 10k polígonos com preenchimento nos convexos, contorno em todos; zoom-out mantém fluidez (LOD).
- [ ] Importar `testdata/bench/bench_points.csv` (SRID 4674): 50k pontos; zoom-in recupera todos (stride zera).
- [ ] Importar `testdata/bench/bench_raster.tif`: banda 1 visível sob o vetor; seletor de banda no inspetor troca a textura; **observar custo de preenchimento GPU com `bench_raster_stress.tif`** (gatilho 3b real).
- [ ] Conectar banco (TA_DATABASE_URL ou diálogo): migrações aplicam, Ctrl+O lista projetos, import persiste (COPY), edição de atributo na tabela persiste (`update_attrs`), delete com confirmação, undo/redo reflete no banco.
- [ ] Status bar: coordenadas do cursor em graus (EPSG:4674), escala 1:N coerente com o zoom.
