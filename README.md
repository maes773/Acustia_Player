# Auralis — v0.0.1 Alpha

Reproductor de música local para Windows, escrito en Rust con Slint (interfaz neumórfica),
Symphonia (decodificación) y CPAL (salida de audio). Sin Electron ni Chromium.

> **Estado:** primera versión funcional. El código de los crates `auralis-audio`,
> `auralis-library`, `auralis-player` y `auralis-app` **todavía no se ha compilado ni
> ejecutado** (se escribió en un entorno sin acceso a crates.io). Los crates sin
> dependencias externas sí tienen tests pasando. Ver «Estado de verificación».

## Ejecutar (Windows)

Requisitos: [Rust](https://rustup.rs) (toolchain `stable-x86_64-pc-windows-msvc`) y
las *Build Tools* de Visual Studio (carga de trabajo «Desarrollo para el escritorio con C++»).

```powershell
cd auralis
cargo run --release -p auralis
```

Una vez abierta la ventana: **Añadir carpeta** → elige una carpeta de música → haz clic en una canción. Puedes añadir tantas carpetas como quieras.

En modo debug (`cargo run`) las dependencias se compilan optimizadas (ver `Cargo.toml`),
pero la ejecución es más lenta; para escuchar música usa `--release`.

## Tema y bandeja

- **Tema claro/oscuro:** botón de luna/sol en la cabecera (no se recuerda entre sesiones).
- **Logo:** `crates/auralis-app/assets/` (`icon.svg` es el original; `icon.ico` y los PNG se generan
  a partir de él). El `.ico` se incrusta en `auralis.exe` al compilar para Windows (requiere el
  Windows SDK, que incluyen las Build Tools de Visual Studio); los PNG se usan en la ventana y la bandeja.
- **Bandeja del sistema (solo Windows):** botón ⌄ de la cabecera. Oculta la ventana y deja un
  icono; clic izquierdo restaura, clic derecho abre el menú (Mostrar, Reproducir/Pausar, Salir).
  La música sigue sonando. Cerrar con la **X** sale de la aplicación.

## Si no suena nada

Haz clic en la línea de estado (abajo a la derecha de la lista) para ver el diagnóstico del audio:

```
<dispositivo> · 48000 Hz · 2 canales · F32 | callbacks N · buffer X% · underruns N · decodificado Ns · reproducido Ns · play
```

- `callbacks` no sube → el dispositivo no está pidiendo audio (revisa el dispositivo predeterminado de Windows).
- `decodificado` = 0 → el archivo no se decodifica (aparece un error en la línea de estado).
- `decodificado` sube pero `reproducido` no → pausa activa o dispositivo detenido.
- `underruns` sube sin parar → el decodificador no da abasto (usa `--release`). El silencio normal
  al terminar la última pista ya no cuenta como underrun.
- Ambos suben pero no se oye → revisa el volumen del reproductor y el mezclador de Windows.

## Pruebas

```powershell
cargo test --workspace
```

## Estructura

```
auralis/
├─ Cargo.toml                  workspace
└─ crates/
   ├─ auralis-core/            cola, shuffle, repeat, modelos, formato de tiempo (sin dependencias)
   ├─ auralis-audio/           motor: Symphonia + CPAL, ring buffer, gapless
   ├─ auralis-library/         escaneo de carpetas, etiquetas, duración, carátulas
   ├─ auralis-player/          controlador: lista + cola + motor (sin UI)
   └─ auralis-app/             interfaz Slint y ejecutable `auralis`
```

Dependencias entre crates: `app → player → (core, audio)`, `app → library → core`.
Solo `auralis-app` conoce Slint y `rfd` (selector de carpeta). Para Android bastaría con
reutilizar `core`, `audio`, `library` y `player` con otra interfaz.

## Qué hace esta versión

- **Varias carpetas** en listas separadas (una pestaña por carpeta, con el nombre de la carpeta; se numeran si coinciden). Se leen al abrirlas por primera vez. Tocar una canción de otra pestaña cambia la reproducción a esa carpeta (♪ marca la que suena); cambiar de pestaña sin tocar nada no interrumpe la música. La × de la pestaña quita la carpeta de la app (no borra archivos).
- **Se guarda entre sesiones:** carpetas, carpeta seleccionada, modo oscuro y nombres editados, en `%APPDATA%\Auralis\settings.txt` (texto plano, escritura atómica). Si una carpeta ya no existe (disco desconectado) sigue en la lista marcada con ⚠ y se reintenta al seleccionarla.
- **Editar el nombre de una canción** con el lápiz de cada fila: solo cambia cómo se ve en Auralis, el archivo no se toca. «Restaurar» vuelve al nombre original. Se guarda por ruta de archivo.
- **Búsqueda** (sin distinguir mayúsculas ni acentos; todas las palabras deben aparecer, en cualquier orden) por nombre mostrado, nombre original, artista y nombre de archivo. Filtra solo la lista visible: no altera la cola de reproducción.
- Lista con nombre, artista si hay etiqueta,
  duración y carátula incrustada (miniatura en la lista, tamaño grande en el reproductor).
- Reproducir/pausar, anterior, siguiente, seek (barra de progreso), volumen, tiempo
  transcurrido y duración.
- **Shuffle** activable/desactivable sin alterar el orden original: al desactivarlo se
  continúa en orden original desde la pista actual. Al elegir una canción con shuffle activo
  se vuelve a barajar dejándola primera.
- **Repeat** con tres estados (clic para ciclar): desactivado → toda la lista → una canción.
- **Anterior:** si llevas más de 3 s reinicia la pista; si no, va a la anterior.
- **Siguiente** al final de la lista con repeat desactivado no hace nada.
- Si una pista no se puede abrir, se muestra el error y se salta a la siguiente.
- Formatos: MP3, FLAC, WAV, OGG Vorbis, AIFF.

Fuera de alcance (versiones futuras): metadatos online, MusicBrainz, edición de etiquetas,
ecualizador, ReplayGain, widgets, animaciones, Android.

## Limitaciones conocidas

Detalle técnico en [`docs/AUDIO.md`](docs/AUDIO.md).

- **Gapless:** depende del formato y de que el archivo traiga la información del codificador
  (MP3 sin cabecera LAME/Xing mantiene el relleno del codificador y puede dejar un pequeño hueco).
  Hay una ventana de ~2 s alrededor de cada cambio de pista en la que cambiar shuffle/repeat
  puede no afectar a la pista ya encadenada.
- **Shuffle + Repeat All:** la lista barajada se repite en el mismo orden (no se rebaraja al dar la vuelta).
- **Remuestreo:** filtro sinc (Kaiser, ≈90 dB de rechazo) cuando la frecuencia del archivo difiere de la del dispositivo (p. ej. 44,1 kHz → 48 kHz); es de buena calidad pero no «audiófilo» y cuesta algo de CPU.
- **Canales:** mono↔estéreo se convierte bien; audio multicanal se reduce copiando los primeros canales (sin downmix envolvente).
- **Duración:** algunos MP3 VBR sin cabecera no informan duración; la barra de progreso no podrá avanzar/seek en ellos.
- **Pausa, volumen y cambios de pista:** usan una rampa de ~12 ms para evitar clics; pausar o cambiar de pista tarda ese tiempo en notarse.
- **Dispositivo de salida:** no hay selector; la app sigue siempre el dispositivo predeterminado de Windows. Lo comprueba cada ~1 s: si cambia (conectas/quitas auriculares) o el stream falla, reabre la salida y retoma la canción en la posición donde iba, con un corte de unas decenas de ms. Si no queda ningún dispositivo, conserva el actual y reintenta; si se abre la app sin ningún dispositivo de audio, hay que reiniciarla al conectar uno.
- **Interfaz:** sin accesibilidad, atajos de teclado ni arrastrar y soltar; el relieve «hundido» se aproxima con degradados (Slint solo admite una sombra por elemento).
- **Ajustes:** los nombres editados se asocian a la ruta del archivo; si mueves o renombras el archivo se pierde ese nombre. Rutas con caracteres no UTF-8 se guardan con pérdida. No se guarda el volumen ni shuffle/repeat.
- **Varias carpetas:** las carpetas leídas permanecen en memoria hasta cerrar la app (con bibliotecas enormes el consumo crece). Shuffle/repeat actúan sobre la carpeta que suena, no sobre todas a la vez. No se vuelve a escanear una carpeta ya leída (reinicia la app para ver archivos nuevos).
- **Búsqueda:** al filtrar se reconstruye la lista y el scroll vuelve arriba. En el diálogo de edición, Esc cancela y Enter guarda (no probado con teclado real).
- La bandeja no se ha probado en Windows (usa el logo; si no pudiera decodificarlo, un icono generado en código).

## Estado de verificación

| Pieza | Estado |
|---|---|
| `auralis-core` (cola, shuffle, repeat, tiempo, ajustes, búsqueda) | 34 tests pasan |
| `catalog.rs` (nombres editados, búsqueda, nombres de pestañas) de `auralis-app` | 11 tests pasan (módulo sin Slint, probado aparte) |
| `main.rs` | Comprobación de tipos y préstamos contra versiones **simuladas** de `slint`, `player`, `library` y `rfd` (sin errores). No es una compilación real |
| `auralis-audio::converter` (canales, remuestreo sinc, vaciado final) | 11 tests pasan |
| Motor de audio real (`output`, `decoder_thread`, `engine`) con dispositivo y lector **simulados** | 7 pruebas de comportamiento pasan: empalme gapless 44,1→48 kHz sin saltos ni pérdida de muestras, sin clics al pausar/mover volumen/cambiar de pista, seek, errores, pista corta. Comprobado que fallan con el comportamiento antiguo |
| `auralis-library::scan` (búsqueda recursiva y orden) | 2 tests pasan (función probada de forma aislada) |
| Detección de cambio de dispositivo/fallo de stream y reanudación en la posición (`Engine`) | 3 pruebas pasan con dispositivo simulado. La lógica de `Player` que las usa y el comportamiento con Windows/WASAPI real **no están probados** |
| Ajustes de segundo plano de Windows (`power.rs`, MMCSS en `platform.rs`) | Solo comprobados de tipos (compilados como si fuera Windows en Linux, sin enlazar). **No probados en Windows**; la causa de los artifacts en bandeja es una hipótesis |
| Sintaxis de todos los `.rs` | Verificada con `rustfmt` (sin tipos ni dependencias) |
| Resto de `auralis-audio`, `auralis-library`, `auralis-player`, `auralis-app`, y `main.slint` | **Sin compilar.** Pueden requerir ajustes menores de API (versiones de `symphonia`, `cpal`, `ringbuf`, `slint`, `rfd`) |
| Reproducción con tu dispositivo real, gapless en archivos reales, UI, bandeja, icono del `.exe` | **Sin probar** (necesita Windows) |
