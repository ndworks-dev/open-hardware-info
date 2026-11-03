# Open Hardware Info

Monitor de recursos que mostra rede, CPU, memória, GPU e disco direto na barra de tarefas do Windows.

Inspirado no [TrafficMonitor](https://github.com/zhongyang219/TrafficMonitor), mas sem driver de kernel: o projeto não usa `WinRing0` nem LibreHardwareMonitor, que é o que faz o Windows Defender bloquear esse tipo de ferramenta. É um único `.exe`, sem DLL própria e sem exigir administrador.

## Como usar

Requer Rust e as ferramentas de build do Visual Studio.

```powershell
cargo build --release
.\target\release\open-hardware-info.exe
```

Clique com o botão direito no widget ou no ícone da bandeja para escolher os itens, o monitor e a posição, ativar o início com o Windows ou sair.

`--print-sample` imprime uma leitura dos sensores, e `--config <arquivo>` usa outro arquivo de configuração.

## Métricas

| Item (`metric`) | Fonte |
| --- | --- |
| `net_down`, `net_up` | `GetIfTable2`, somando as interfaces físicas ativas |
| `cpu` | Contador de desempenho `% Processor Utility` (PDH) |
| `ram` | `GlobalMemoryStatusEx` |
| `disk` | Contador de desempenho `% Idle Time` (PDH) |
| `gpu`, `gpu_temp`, `vram` | NVML, instalado com o driver da NVIDIA |

Sem GPU NVIDIA, os itens de GPU mostram `--`. A temperatura da CPU não está disponível, porque ler esse sensor exige um driver de kernel.

## Configuração

O arquivo fica em `%APPDATA%\OpenHardwareInfo\config.toml` e é criado quando você muda algo pelo menu ou escolhe "Editar configurações...". As alterações são aplicadas sem reiniciar. Se o arquivo tiver um erro, o app mantém a última configuração válida e avisa na dica do ícone da bandeja.

```toml
interval_ms = 1000       # intervalo de leitura
monitor = "primary"      # ou "DISPLAY1"
anchor = "right"         # ou "left"
offset = 8               # distância da borda ou da bandeja
secondary_reserve = 190  # espaço livre à direita em barras secundárias
rows = 2                 # linhas por coluna
font = "Segoe UI"
font_size = 9.0
text_color = "auto"      # segue o tema do Windows, ou "#RRGGBB"
background = "transparent" # ou "#RRGGBBAA"
adapter = ""             # parte do nome de um adaptador; vazio soma todos

[[items]]                # a ordem aqui é a ordem na tela
metric = "cpu"
label = "CPU"            # opcional
color = "#FFB000"        # opcional
```

## Como funciona

O widget é uma janela filha da barra de tarefas (`Shell_TrayWnd` ou `Shell_SecondaryTrayWnd`), sem injeção de código nem hooks. Assim, ele acompanha a barra e some junto com ela em tela cheia. O Windows não tem uma API oficial para isso, então uma atualização do sistema pode exigir ajustes.

As leituras rodam em uma thread separada, porque a janela compartilha a fila de entrada com o Explorer e não pode travar. A cada leitura, o app também confere a barra, o monitor e o tema, e recria o widget se o Explorer reiniciar.

## Testes

```powershell
cargo test
```
