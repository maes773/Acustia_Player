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

Una vez abierta la ventana: **Abrir carpeta** → elige tu carpeta de música → haz clic en una canción.

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

- Selección de carpeta (búsqueda recursiva) y lista con nombre, artista si hay etiqueta,
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
ecualizador, ReplayGain, widgets, bandeja del sistema, temas, animaciones, Android.

## Limitaciones conocidas

Detalle técnico en [`docs/AUDIO.md`](docs/AUDIO.md).

- **Gapless:** depende del formato y de que el archivo traiga la información del codificador
  (MP3 sin cabecera LAME/Xing mantiene el relleno del codificador y puede dejar un pequeño hueco).
  Hay una ventana de ~1 s alrededor de cada cambio de pista en la que cambiar shuffle/repeat
  puede no afectar a la pista ya encadenada.
- **Shuffle + Repeat All:** la lista barajada se repite en el mismo orden (no se rebaraja al dar la vuelta).
- **Remuestreo:** filtro sinc (Kaiser, ≈90 dB de rechazo) cuando la frecuencia del archivo difiere de la del dispositivo (p. ej. 44,1 kHz → 48 kHz); es de buena calidad pero no «audiófilo» y cuesta algo de CPU.
- **Canales:** mono↔estéreo se convierte bien; audio multicanal se reduce copiando los primeros canales (sin downmix envolvente).
- **Duración:** algunos MP3 VBR sin cabecera no informan duración; la barra de progreso no podrá avanzar/seek en ellos.
- **Pausa, volumen y cambios de pista:** usan una rampa de ~12 ms para evitar clics; pausar o cambiar de pista tarda ese tiempo en notarse.
- **Dispositivo de salida:** se usa el predeterminado al arrancar; no hay selector ni reacción a cambios (p. ej. desconectar auriculares).
- **Interfaz:** sin accesibilidad, atajos de teclado ni arrastrar y soltar; el relieve «hundido» se aproxima con degradados (Slint solo admite una sombra por elemento).
- No se recuerda la última carpeta ni el tema entre sesiones.
- La bandeja no se ha probado en Windows (usa el logo; si no pudiera decodificarlo, un icono generado en código).

## Estado de verificación

| Pieza | Estado |
|---|---|
| `auralis-core` (cola, shuffle, repeat, tiempo) | 15 tests pasan |
| `auralis-audio::converter` (canales, remuestreo sinc, vaciado final) | 11 tests pasan |
| Motor de audio real (`output`, `decoder_thread`, `engine`) con dispositivo y lector **simulados** | 7 pruebas de comportamiento pasan: empalme gapless 44,1→48 kHz sin saltos ni pérdida de muestras, sin clics al pausar/mover volumen/cambiar de pista, seek, errores, pista corta. Comprobado que fallan con el comportamiento antiguo |
| `auralis-library::scan` (búsqueda recursiva y orden) | 2 tests pasan (función probada de forma aislada) |
| Sintaxis de todos los `.rs` | Verificada con `rustfmt` (sin tipos ni dependencias) |
| Resto de `auralis-audio`, `auralis-library`, `auralis-player`, `auralis-app`, y `main.slint` | **Sin compilar.** Pueden requerir ajustes menores de API (versiones de `symphonia`, `cpal`, `ringbuf`, `slint`, `rfd`) |
| Reproducción con tu dispositivo real, gapless en archivos reales, UI, bandeja, icono del `.exe` | **Sin probar** (necesita Windows) |
