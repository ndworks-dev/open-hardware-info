# Open Hardware Info

Monitor de recursos que mostra rede, CPU, memória, GPU e disco direto na barra de tarefas do Windows.

Inspirado no [TrafficMonitor](https://github.com/zhongyang219/TrafficMonitor), mas sem driver próprio: o projeto não usa `WinRing0` nem LibreHardwareMonitor, que é o que faz o Windows Defender bloquear esse tipo de ferramenta. O app é um único `.exe`, sem DLL própria e sem exigir administrador. Só dois recursos opcionais precisam de um pequeno serviço: a temperatura da CPU, que também usa o driver [PawnIO](https://pawnio.eu), e a curva dos fans da GPU.

## Instalação

No PowerShell, sem precisar de administrador:

```powershell
irm https://github.com/ndworks-dev/open-hardware-info/releases/latest/download/install.ps1 | iex
```

O mesmo comando atualiza uma instalação existente. O instalador baixa a última release, confere o SHA-256, instala em `%LOCALAPPDATA%\Programs\OpenHardwareInfo`, cria um atalho no menu Iniciar e abre o app.

Para instalar uma versão específica ou desinstalar:

```powershell
& ([scriptblock]::Create((irm https://github.com/ndworks-dev/open-hardware-info/releases/latest/download/install.ps1))) -Version 0.2.0
& ([scriptblock]::Create((irm https://github.com/ndworks-dev/open-hardware-info/releases/latest/download/install.ps1))) -Uninstall
```

A desinstalação mantém suas configurações em `%APPDATA%\OpenHardwareInfo`.

### Temperatura da CPU (opcional)

Ler esse sensor exige acesso ao kernel, então a temperatura depende de dois componentes que o app não instala sozinho: o driver PawnIO e o serviço `ohi-sensor`, em `%ProgramFiles%\OpenHardwareInfo`. O app continua sem privilégios e só lê o valor que o serviço publica.

Na primeira instalação, o instalador pergunta se você quer esses componentes. Depois, use o link "Instalar" na linha "Temperatura da CPU", na aba Itens das configurações. Os dois caminhos pedem confirmação e permissão de administrador, e baixam os arquivos do GitHub.

Para instalar ou remover pela linha de comando, abra o PowerShell como administrador:

```powershell
& ([scriptblock]::Create((irm https://github.com/ndworks-dev/open-hardware-info/releases/latest/download/install.ps1))) -InstallCpuTemp
& ([scriptblock]::Create((irm https://github.com/ndworks-dev/open-hardware-info/releases/latest/download/install.ps1))) -UninstallCpuTemp
```

Depois de instalar, ative "Temperatura da CPU" nas configurações. A remoção mantém o PawnIO, porque outros programas podem usá-lo; para tirá-lo, use as configurações do Windows. Isso deixa o pacote do driver no sistema: para removê-lo também, encontre o `pawnio.inf` em `pnputil /enum-drivers` e rode `pnputil /delete-driver oemNN.inf` como administrador, usando o nome publicado que aparecer ali.

Alguns anti-cheats podem bloquear jogos com o PawnIO instalado. Funciona em processadores AMD Ryzen e Intel, e a leitura em Intel ainda não foi testada em hardware real.

### Curva dos fans da GPU (opcional)

A aba Fans das configurações controla os fans de GPUs NVIDIA por uma curva de temperatura e velocidade, como a do MSI Afterburner. O Windows exige permissão de administrador para mudar um fan, então a curva depende do mesmo serviço `ohi-sensor`, mas não do PawnIO. O app continua sem privilégios e só publica a curva para o serviço.

Na primeira vez, use o link "Instalar" na aba Fans, ou abra o PowerShell como administrador:

```powershell
& ([scriptblock]::Create((irm https://github.com/ndworks-dev/open-hardware-info/releases/latest/download/install.ps1))) -InstallFanControl
& ([scriptblock]::Create((irm https://github.com/ndworks-dev/open-hardware-info/releases/latest/download/install.ps1))) -UninstallFanControl
```

O serviço é o mesmo da temperatura da CPU, então removê-lo desliga os dois recursos.

- A curva só vale com o app aberto. Se ele fechar ou travar, a placa volta ao próprio controle automático em até 10 segundos.
- Abaixo do primeiro ponto, a placa decide, então ela ainda pode parar os fans em baixa temperatura.
- A partir de 90 °C, os fans vão a 100%, seja qual for a curva, até a GPU voltar a menos de 85 °C.
- Se o próprio serviço travar, o Windows tenta reiniciá-lo duas vezes, e ele devolve o controle à placa ao iniciar. Se isso falhar, os fans podem ficar na última velocidade até reiniciar o Windows.
- Velocidade baixa demais pode danificar a GPU. Não use junto com o MSI Afterburner ou outro programa que controle os fans, porque eles disputam o controle.

Só a primeira GPU NVIDIA é controlada.

## Como usar

Clique com o botão direito no widget ou no ícone da bandeja para abrir o menu rápido. Para ajustar tudo, escolha "Configurações..." no menu ou dê dois cliques no ícone da bandeja. As mudanças aparecem na barra enquanto você edita.

`--print-sample` imprime uma leitura dos sensores, `--settings` abre só a janela de configurações, `--quit` fecha o app em execução, e `--config <arquivo>` usa outro arquivo de configuração.

## Métricas

| Item (`metric`) | Fonte |
| --- | --- |
| `net_down`, `net_up` | `GetIfTable2`, somando as interfaces físicas ativas |
| `cpu` | Contador de desempenho `% Processor Utility` (PDH) |
| `cpu_temp` | Serviço `ohi-sensor`, com o driver PawnIO |
| `ram` | `GlobalMemoryStatusEx` |
| `disk` | Contador de desempenho `% Idle Time` (PDH) |
| `gpu`, `gpu_temp`, `vram` | NVML, instalado com o driver da NVIDIA |

Sem GPU NVIDIA, os itens de GPU mostram `--`. Sem o serviço rodando, `cpu_temp` também mostra `--`.

## Configuração

A janela de configurações cobre todas as opções abaixo. Elas ficam em `%APPDATA%\OpenHardwareInfo\config.toml`, criado na primeira mudança, e o arquivo também pode ser editado à mão, sem reiniciar. Se ele tiver um erro, o app mantém a última configuração válida, avisa na dica do ícone da bandeja, e a janela não salva nada até o erro ser corrigido ou os padrões serem restaurados.

```toml
interval_ms = 1000       # intervalo de leitura
monitor = "primary"      # ou "DISPLAY1"
anchor = "right"         # ou "left"
offset = 8               # distância da borda ou da bandeja
secondary_reserve = 190  # espaço livre à direita em barras secundárias
rows = 2                 # 1 = lado a lado; 2 ou 3 = um em cima do outro
font = "system"          # fonte do Windows, ou o nome de uma fonte instalada
font_size = 9.0
font_weight = "regular"  # ou "semibold", "bold"
text_color = "auto"      # segue o tema do Windows, ou "#RRGGBB"
background = "transparent" # ou "#RRGGBBAA"
adapter = ""             # parte do nome de um adaptador; vazio soma todos

[gpu_fan]
enabled = false          # a aba Fans liga isto depois de instalar o serviço
points = [[50, 30], [60, 40], [70, 60], [80, 80], [85, 100]] # [°C, % do fan]

[[items]]                # a ordem aqui é a ordem na tela
metric = "cpu"
label = "CPU"            # opcional
color = "#FFB000"        # opcional
```

## Como funciona

O widget é uma janela filha da barra de tarefas (`Shell_TrayWnd` ou `Shell_SecondaryTrayWnd`), sem injeção de código nem hooks. Assim, ele acompanha a barra e some junto com ela em tela cheia. O Windows não tem uma API oficial para isso, então uma atualização do sistema pode exigir ajustes.

As leituras rodam em uma thread separada, porque a janela compartilha a fila de entrada com o Explorer e não pode travar. A cada leitura, o app também confere a barra, o monitor e o tema, e recria o widget se o Explorer reiniciar.

A janela de configurações é feita com [Slint](https://slint.dev) e roda em um processo separado do mesmo `.exe`, que só existe enquanto ela está aberta. Ela grava o `config.toml`, e o widget aplica cada alteração. O Slint é usado sob a licença Royalty-free 2.0, com a atribuição exibida na aba "Sobre".

## Compilação

Requer Rust, as ferramentas de build do Visual Studio e o Windows SDK.

```powershell
cargo build --release
.\target\release\open-hardware-info.exe
cargo test
```

## Releases

`.github/workflows/release.yml` compila, verifica e testa cada push e pull request. Quando um push na `main` contém commits que justificam uma release, ele cria a tag `vX.Y.Z` e publica uma release no GitHub com:

- `OpenHardwareInfo-X.Y.Z-windows-x64.zip`: o executável e os textos das licenças, inclusive os das dependências
- `install.ps1`
- `SHA256SUMS.txt`

A versão sai dos [Conventional Commits](https://www.conventionalcommits.org/) desde a última tag (`scripts/release-version.sh`):

| Commits desde a última release | Próxima versão |
| --- | --- |
| Mudança incompatível (`feat!:` ou rodapé `BREAKING CHANGE:`) | major; minor enquanto estiver abaixo de 1.0 |
| Pelo menos um `feat` | minor |
| `fix` ou `perf` | patch |
| Só `docs`, `chore`, `ci`, `refactor`, `test`, `build` | sem release |

A primeira release usa a versão do `Cargo.toml`. Para pular para uma versão específica, aumente-a no `Cargo.toml`: uma versão manual maior sempre vence. A CI grava a versão da release no executável, sem alterar o `Cargo.toml` do repositório. Releases são feitas só pela CI, nunca crie tags `v*` à mão.

## Licença

[MIT](LICENSE). A janela de configurações usa o [Slint](https://slint.dev) sob a licença Royalty-free 2.0, que tem termos próprios.
