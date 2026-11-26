# Acustia Player — v0.0.6 Alpha (antes «Auralis»)

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

- **Tema claro/oscuro:** botón de luna/sol en la cabecera (se recuerda entre sesiones).
- **Apariencia (botón del círculo de color junto a la búsqueda):**
  - **Ambient Light:** el acento, el fondo (teñido al menos un 35 %) y un resplandor tras el CD toman el color dominante de la carátula de la canción que suena, con un fundido de 0,6 s al cambiar. El color se calcula muestreando ~4000 píxeles (coste despreciable) y se ignoran grises, casi negros y píxeles transparentes; si la carátula es gris o no hay, se usa tu tema. Se puede desactivar con un interruptor.
  - **Temas configurables:** modo base claro u oscuro, 8 presets (Índigo —el original—, Océano, Menta, Bosque, Ámbar, Atardecer, Rosa, Violeta), un deslizador de **color de acento** (matiz) y otro de **tinte del fondo** (0 = el gris-azul original; 100 = fondo del color elegido). El acento se ajusta solo para tener contraste suficiente (un amarillo sigue leyéndose sobre blanco). Con Ambient Light activo y una carátula con color, los controles manuales se atenúan porque no tienen efecto hasta que lo desactives. «Restablecer» vuelve a Índigo sin tinte.
- **Atajos globales de teclado (Windows):** funcionan con la ventana en segundo plano o en la bandeja. Teclas multimedia (reproducir/pausar, siguiente, anterior, detener) y `Ctrl+Alt+Espacio` reproducir/pausar, `Ctrl+Alt+→/←` siguiente/anterior, `Ctrl+Alt+↑/↓` volumen ±5 %, `Ctrl+Alt+S` aleatorio, `Ctrl+Alt+R` repetir (lista → canción → no), `Ctrl+Alt+A` mostrar la ventana. Interruptor para desactivarlos en el panel de apariencia; si otra aplicación ya tiene alguno (p. ej. Spotify con las teclas multimedia), el panel lo avisa y el resto sigue funcionando.
- **Logo:** `crates/auralis-app/assets/` (`icon.svg` es el original; `icon.ico` y los PNG se generan
  a partir de él). El `.ico` se incrusta en `acustia-player.exe` al compilar para Windows (requiere el
  Windows SDK, que incluyen las Build Tools de Visual Studio); los PNG se usan en la ventana y la bandeja.
- **Bandeja del sistema (solo Windows):** botón ⌄ de la cabecera. Oculta la ventana y deja un
  icono; clic izquierdo restaura, clic derecho abre el menú: canción actual, Reproducir/Pausar, **Anterior**, **Siguiente**, **Aleatorio** (con marca), **Repetir** (no / toda la lista / una canción) , Mostrar y Salir. El menú y el texto del icono se actualizan solos con lo que suena.
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
- **Se guarda entre sesiones:** carpetas, carpeta seleccionada, modo oscuro y nombres editados, en `%APPDATA%\Acustia Player\settings.txt` (texto plano, escritura atómica). Si una carpeta ya no existe (disco desconectado) sigue en la lista marcada con ⚠ y se reintenta al seleccionarla.
- **Editar metadatos** con el lápiz de cada fila: título, artista, álbum, género, año y **carátula** (elegir JPG/PNG o quitarla). Se **incrustan en el archivo** (ID3 en MP3/AIFF/WAV, Vorbis en FLAC/OGG) con `lofty`, en un hilo aparte, y solo se escribe lo que cambió. La carátula se guarda tal cual si es JPEG/PNG de ≤900 KB; si no, se reduce a ≤1000 px JPEG q85. Si se edita la canción que suena, se pausa un instante mientras se escribe y se reanuda en la misma posición. Si el archivo no se puede modificar (solo lectura, formato sin soporte de etiquetas), el botón «Solo en la app» guarda el nombre sin tocar el archivo.
- **Detección automática de cambios en carpetas:** cada carpeta leída se vigila (sondeo cada 3 s, sin dependencias nativas). Las canciones nuevas aparecen solas y las borradas desaparecen; no se interrumpe la música ni se pierden nombres/carátulas. Se espera a que un archivo deje de crecer (1,5 s) antes de añadirlo, para no leer copias a medias. Una carpeta no disponible (disco desconectado) no se interpreta como vacía.
- **Modo widget:** botón de la ventana pequeña (junto a la luna/sol) o «Pasar a modo widget ahora» en Ajustes. La ventana pasa a un panel compacto de 380×150 sin marco (CD o cassette, título, artista, barra de progreso, anterior/play/siguiente, volver a la ventana completa y ocultar a la bandeja). Se **arrastra desde cualquier parte libre** y **se queda donde la sueltes**: la posición se guarda en los ajustes y se restaura al volver a abrirlo. Opcionalmente «Widget siempre encima de otras ventanas».
- **Ajustes** (engranaje junto a la búsqueda): «Abrir siempre en modo widget» y «Iniciar Acustia al arrancar el equipo» (entrada `HKCU\...\Run` creada con `reg.exe`, sin permisos de administrador; se renueva la ruta en cada arranque por si mueves el programa).
- **Modo podcast por carpeta:** casilla «Modo podcast (esta carpeta)» sobre la búsqueda. Es independiente en cada carpeta (una puede ser música y otra podcast; las pestañas de podcast llevan la marca POD). Lo que suena desde una carpeta de podcast muestra un **cassette** en vez del CD: las dos bobinas giran mientras suena y la cinta pasa de la bobina izquierda a la derecha según el progreso del episodio.
- **Estadísticas de escucha** (botón de barras junto a la búsqueda): tiempo escuchado hoy / últimos 7 días / este mes / este año, total de música y de **horas de podcast**, promedio por día, días con escucha y racha, número de canciones locales y episodios, reproducciones contadas, **artista**, **canción** y podcast más escuchados, top 8 de canciones y de artistas y un gráfico de los últimos 14 días. Se guardan en `%APPDATA%\Acustia Player\stats.txt`. «Reiniciar» las borra.
- **Disco neumórfico giratorio** en el reproductor, con la carátula como etiqueta del CD; gira mientras suena y se detiene al pausar (disco plateado liso si no hay carátula).
- **Renombrado a Acustia Player:** los ajustes de la versión anterior (`%APPDATA%\Auralis`) se copian automáticamente a `%APPDATA%\Acustia Player`.
- **Ecualizador de 10 bandas** (botón con los tres deslizadores, junto a la búsqueda): 31 Hz a 16 kHz, ±12 dB en pasos de 0,5 dB, con 12 presets (Plano, Bajos potenciados, Agudos potenciados, Rock, Pop, Jazz, Clásica, Electrónica, Hip-Hop, Voz, Acústico, Noche). Mover una banda o elegir un preset lo activa; si las bandas no coinciden con ningún preset aparece «Personalizado». Funciona en el callback de audio, así que se oye al instante y los cambios se interpolan (sin crujidos). Un *preamp* automático compensa el máximo de la respuesta combinada para que subir bandas no sature, y un limitador suave protege los picos. Se guarda en los ajustes.
- **ReplayGain:** con «Normalizar volumen» activado, si el archivo trae etiquetas `REPLAYGAIN_TRACK_GAIN/PEAK` (o `ALBUM`) se usan directamente (referencia ReplayGain 2.0 de −18 LUFS desplazada a los −14 LUFS de la app, y limitada para que el pico no pase de −1 dBFS); sin etiquetas se sigue midiendo la sonoridad como antes. Un interruptor en el panel de audio elige la ganancia de **álbum** (conserva la dinámica entre las canciones de un disco) en vez de la de pista.
- **Fundidos al pausar, reanudar y cambiar de canción:** con el crossfade activado, pausar o reanudar ya no corta de golpe: el audio baja/sube con la duración del crossfade (tope 1 s). Al pasar a otra canción a mano (siguiente/anterior/elegir) la actual baja a silencio (tope 1,5 s) y la nueva sube (tope 0,7 s). Mover la barra de progreso mantiene un fundido mínimo para responder al instante. Con el crossfade en «No» todo sigue siendo inmediato (rampa anticlic de 12 ms).
- **Favoritos:** estrella en cada fila (se rellena al marcarla; se guarda por ruta del archivo en los ajustes) y botón ★ junto a la búsqueda para ver **solo las favoritas** de la carpeta que estás viendo.
- **Historial de reproducción:** botón del reloj junto a la búsqueda. Guarda las últimas 300 canciones que han empezado a sonar (más reciente primero, con «hace 5 min»), en `%APPDATA%\Acustia Player\history.txt`. Pulsar una entrada la reproduce (cambia a su carpeta); las que ya no están en ninguna carpeta aparecen atenuadas. «Borrar todo» vacía el historial.
- **Normalización de volumen** (interruptor «Normalizar volumen», activado por defecto): mide la sonoridad de cada canción (LUFS, norma EBU R128 / BS.1770) y la lleva a −14 LUFS (el nivel «Normal» de Spotify), de modo que no haya que subir o bajar el volumen entre canciones. La **barra de volumen sigue siendo el control maestro**: la normalización solo iguala el punto de partida de cada canción y nunca sube una canción más allá de −1 dBFS de pico (no genera distorsión). La primera vez que suena una canción se mide al vuelo con sus primeros 45 s; una medición completa se hace en segundo plano a baja prioridad y se guarda en `%APPDATA%\Acustia Player\loudness.txt`.
- **Crossfade configurable** (botones − / + , de 0 a 12 s; 0 = desactivado, que es el gapless de siempre): al final de cada canción la siguiente entra mientras la actual se desvanece (potencia constante). La **mezcla** (las dos suenan a la vez) solo ocurre en el cambio automático; en los cambios manuales y la pausa hay fundido de salida y de entrada (ver arriba). Se guarda en los ajustes.
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

Fuera de alcance (versiones futuras): metadatos online, MusicBrainz,
Android, vistas por artista/álbum/género.

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
- **Normalización:** las canciones cuyo formato no permite medir (archivo ilegible o silencio) suenan sin ajuste. Una canción se mide completa en segundo plano, así que su ganancia puede variar levemente entre la primera reproducción (45 s) y las siguientes. Al activar/desactivar el interruptor el cambio se oye con hasta ~2 s de retraso (audio ya en cola) y con una rampa suave. Con muchos miles de canciones el análisis en segundo plano tarda en completarse (usa baja prioridad).
- **Crossfade:** necesita conocer la duración de la canción (MP3 sin cabecera Xing/LAME y algunos OGG no la informan: ahí se encadena sin mezclar). Una canción que entra no puede mezclarse durante más de la mitad de su duración. Con canciones muy cortas (< unos segundos más que el crossfade) la mezcla se acorta o no se hace. En la zona mezclada hay un limitador suave por si las dos canciones suman más de 0 dBFS.
- **Edición de metadatos:** escribir en el archivo necesita permiso de escritura y que ningún otro programa lo tenga bloqueado. Con MP3/FLAC/OGG/WAV/AIFF se usan etiquetas estándar (ID3v2, Vorbis comments); las carátulas en OGG se guardan como `METADATA_BLOCK_PICTURE`. Un fallo a mitad de escritura podría dejar el archivo dañado (como con cualquier editor de etiquetas): haz copia de seguridad de lo importante en esta versión alpha. Si editas la canción que suena hay un corte breve en el audio.
- **Ecualizador:** 10 bandas fijas (no hay bandas paramétricas ni guardar presets propios). Funciona sobre la salida del dispositivo, así que no se puede comparar «con/sin» sin oír el cambio de nivel del preamp; con presets muy marcados el preamp automático baja el volumen general (es lo esperado: evita saturar). A frecuencias de muestra bajas (< 36 kHz) la banda de 16 kHz no se aplica.
- **ReplayGain:** solo se **leen** las etiquetas; la app no escribe etiquetas ReplayGain en tus archivos (la medición propia queda en `loudness.txt`). Las etiquetas antiguas de ReplayGain 1.0 (referencia 89 dB SPL) se interpretan como las de 2.0 (−18 LUFS), así que pueden quedar unos dB distintas. El modo álbum usa la etiqueta de álbum del archivo; no calcula la ganancia de álbum por su cuenta.
- **Fundidos:** pausar tarda hasta 1 s en silenciar y cambiar de canción a mano hasta ~2 s en completarse cuando el crossfade es largo (con «No» es inmediato). Al cambiar de canción a mano no se solapan las dos canciones (baja una y luego sube la otra): solapar exigiría mantener dos decodificadores sobre el audio ya en cola.
- **Favoritos e historial:** el filtro ★ actúa sobre la carpeta que estás viendo (no hay una lista única de favoritos de todas las carpetas) y la cola sigue siendo la carpeta entera, como con la búsqueda: al reproducir desde la vista de favoritas, «siguiente» puede pasar a una no favorita. Los favoritos se asocian a la ruta del archivo (si lo mueves o renombras se pierde la marca). El historial guarda 300 entradas y no cuenta las recargas de la misma canción.
- **Ajustes:** los nombres editados solo en la app se asocian a la ruta del archivo; si mueves o renombras el archivo se pierde ese nombre. Rutas con caracteres no UTF-8 se guardan con pérdida. No se guarda el volumen ni shuffle/repeat (sí normalización y crossfade).
- **Varias carpetas:** las carpetas leídas permanecen en memoria hasta cerrar la app (con bibliotecas enormes el consumo crece). Shuffle/repeat actúan sobre la carpeta que suena, no sobre todas a la vez. La vigilancia de carpetas es por sondeo cada 3 s (no instantánea) y solo detecta altas/bajas de archivos, no cambios de etiquetas hechos desde otro programa; en carpetas de red lentas o con cientos de miles de archivos el sondeo cuesta algo de E/S.
- **Búsqueda:** al filtrar se reconstruye la lista y el scroll vuelve arriba. En el diálogo de edición, Esc cancela y Enter guarda (no probado con teclado real).
- La bandeja no se ha probado en Windows (usa el logo; si no pudiera decodificarlo, un icono generado en código).

- **Apariencia y atajos:** las combinaciones de teclas son fijas (no hay editor de atajos propios) y solo Windows. Los fundidos de color dependen de la animación de Slint; en equipos muy lentos el resplandor (desenfoque de 90 px, con la capa en caché) es lo más costoso de dibujar: se puede desactivar con Ambient Light. El color dominante es una heurística (carátulas con muchos colores pueden dar un tono «medio» poco representativo).
- **Modo widget:** no se puede «clavar» debajo de las demás ventanas como un gadget del escritorio (Slint no lo permite); el widget es una ventana normal, y «siempre encima» es la alternativa. El cambio de ventana con marco a sin marco en caliente, la restauración de la posición y el arrastre dependen del backend de Slint/Windows y **no se han probado**; si el arrastre va a saltos o el widget aparece fuera de pantalla tras quitar un monitor, avísame. La posición se guarda en píxeles físicos y no se comprueba contra los monitores conectados.
- **Estadísticas:** empiezan a contar desde esta versión (no hay datos anteriores). Cuenta el tiempo realmente sonando (la pausa no suma); una canción cuenta como «reproducción» tras 30 s escuchados y se vuelve a contar al repetirla desde el principio. Se agrupan por día **local** (la zona horaria se pide a Windows). «Canciones locales» y «Episodios» cuentan solo las carpetas que ya se han leído en esta sesión. El «artista/podcast» sale de la etiqueta de artista de cada archivo (en podcasts, normalmente el nombre del programa); sin etiqueta no entra en esos rankings. Se recuerdan como máximo 5000 canciones distintas (se descartan las menos escuchadas).
- **Modo podcast:** solo cambia la animación (cassette) y de qué lado cuentan las estadísticas; no recuerda la posición por episodio ni cambia la velocidad de reproducción.
- **Inicio con el equipo:** usa `reg.exe` (solo Windows). Si mueves el `.exe`, se actualiza la ruta la siguiente vez que lo abras. Si tu antivirus bloquea escribir en `Run`, el ajuste mostrará el error.

## Estado de verificación

| Pieza | Estado |
|---|---|
| `catalog.rs` (nombres editados, búsqueda, sincronización de lista con archivos nuevos/borrados, formulario de metadatos) de `auralis-app` | 15 tests pasan (módulo sin Slint, probado aparte) |
| `auralis-library::watch` (detección de altas/bajas, espera a que termine una copia, carpeta no disponible ≠ vacía) | 3 tests pasan con carpetas temporales reales |
| **`main.slint` (esta entrega: cassette, vista de widget, casilla podcast, paneles de ajustes y estadísticas; antes: tema, resplandor, apariencia, estrella, ★, historial, ecualizador)** | **NO compilado**: no hay compilador de Slint aquí. Las partes nuevas se revisaron a mano; el riesgo está en detalles de sintaxis/propiedades (p. ej. el deslizador vertical `EqBand`, `Flickable`, `ListView` del historial). Las anteriores sí las compilaste tú |
| `auralis-core` (cola, shuffle, repeat, tiempo, búsqueda, ajustes, ecualizador/presets, historial, favoritos, paleta de tema, color dominante, tabla de atajos, **estadísticas y ajustes de widget/podcast/inicio**) | 72 tests pasan (los 10 nuevos: calendario civil, zona horaria, ventanas de tiempo hoy/semana/mes/año, rankings, racha, formato de tiempos, contador de reproducciones, guardado/lectura con texto raro o corrupto, poda). Los de paleta comprueban, para los 360° de matiz × 3 tintes × claro/oscuro, contraste del acento ≥ 3, del texto ≥ 6, y que la luz del relieve siga siendo más clara y la sombra más oscura que el fondo; y que la configuración por defecto reproduce exactamente los colores originales |
| Ecualizador (`eq.rs`) | 8 pruebas: plano = transparente, ±dB en la banda elegida (medido con senos), bandas lejanas intactas, sin saltos al mover, bypass exacto al desactivar, preamp sin sobrepasar el nivel original con +12 dB en todas las bandas; además 1 prueba con el motor completo (−12 dB a 1 kHz medido a la salida) |
| ReplayGain (`replaygain.rs`) | 7 pruebas de lectura de etiquetas, desplazamiento de referencia, límite por pico, modo pista/álbum, límites y respaldo a la sonoridad medida; 1 prueba con el motor completo (pista −2 dB, álbum +2 dB). **La lectura de las etiquetas con Symphonia (`reader.rs`) no está compilada contra el crate real** |
| Fundidos de pausa/cambio de pista (motor real con dispositivo simulado) | 4 pruebas: el fundido de pausa/reanudación dura lo esperado (nivel a la mitad a mitad del fundido, silencio al final, sin escalones), el cambio manual pasa por silencio sin saltos y la pista nueva no se recorta, sin crossfade la pausa es casi inmediata, el seek no espera al crossfade. Las 21 pruebas del motor pasan |
| **`hotkeys.rs` y la parte nueva de `tray.rs` (`global-hotkey` 0.6, `CheckMenuItem`)** | **NO compilados** (solo existen en Windows y no hay red para bajar las librerías); escritos contra la API documentada. Lo primero que probar: que las teclas multimedia respondan y que el menú de la bandeja muestre la marca de «Aleatorio» |
| **`autostart.rs`, `localtime.rs`** | Solo Windows (`reg.exe`, `GetTimeZoneInformation`): **no probados en Windows**; en Linux solo se compilan los reemplazos. Lo primero que probar: que «Iniciar con el equipo» cree la entrada en `Run` y que el día de las estadísticas cambie a medianoche local |
| `main.rs` | Comprobación de tipos y préstamos contra versiones **simuladas** de `slint`, `player`, `library` y `rfd` (sin errores). No es una compilación real |
| `auralis-audio::converter` (canales, remuestreo sinc, vaciado final) | 11 tests pasan |
| Motor de audio real (`output`, `decoder_thread`, `engine`) con dispositivo y lector **simulados** | 7 pruebas (más las nuevas de arriba) de comportamiento pasan: empalme gapless 44,1→48 kHz sin saltos ni pérdida de muestras, sin clics al pausar/mover volumen/cambiar de pista, seek, errores, pista corta. Comprobado que fallan con el comportamiento antiguo |
| `auralis-library::scan` (búsqueda recursiva y orden) | 2 tests pasan (función probada de forma aislada) |
| Detección de cambio de dispositivo/fallo de stream y reanudación en la posición (`Engine`) | 3 pruebas pasan con dispositivo simulado. La lógica de `Player` que las usa y el comportamiento con Windows/WASAPI real **no están probados** |
| Ajustes de segundo plano de Windows (`power.rs`, MMCSS en `platform.rs`) | Solo comprobados de tipos (compilados como si fuera Windows en Linux, sin enlazar). **No probados en Windows**; la causa de los artifacts en bandeja es una hipótesis |
| Medidor de sonoridad (`loudness.rs`) | 10 pruebas: referencia EBU Tech 3341 (997 Hz a −23 dBFS = −23,0 LUFS a 44,1/48/96 kHz), +6 dB = +6 LU, ponderación K, compuertas, caché |
| Normalización y crossfade en el motor real (con dispositivo y lector simulados) | 5 pruebas: dos pistas con 20 dB de diferencia quedan a ≤ 1 dB; el pico no sube; el crossfade dura lo esperado (3 s + 3 s − 1 s = 5 s), sin saltos y anuncia la pista nueva al empezar la mezcla; crossfade 0 = gapless; sin siguiente o con siguiente corrupta no se cuelga. Comprobado que fallan si se desactiva cada función |
| Sintaxis de todos los `.rs` | Verificada con `rustfmt` (sin tipos ni dependencias) |
| Resto de `auralis-audio`, `auralis-library`, `auralis-player`, `auralis-app`, y `main.slint` | **Sin compilar.** Pueden requerir ajustes menores de API (versiones de `symphonia`, `cpal`, `ringbuf`, `slint`, `rfd`) |
| Reproducción con tu dispositivo real, gapless en archivos reales, UI, bandeja, icono del `.exe` | **Sin probar** (necesita Windows) |
