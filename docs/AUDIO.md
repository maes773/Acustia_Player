# Motor de audio — notas técnicas (v0.0.1 Alpha)

## Flujo

```
UI ──Cmd──▶ hilo decodificador ──f32──▶ ring buffer (2 s) ──▶ callback CPAL
 ▲            Symphonia + Converter        ringbuf (SPSC)        (tiempo real)
 └── Engine::poll(): posición (atómico) y eventos ◀──────────────────┘
```

- **Callback CPAL:** solo usa atómicos y el consumidor del ring buffer. Sin locks, sin E/S,
  sin reservas de memoria en el camino normal. Aplica volumen, cuenta fotogramas
  reproducidos y rellena con silencio si hay *underrun*.
- **Hilo decodificador (`auralis-decoder`):** único que abre y lee archivos. Convierte canales y
  frecuencia a la configuración del dispositivo y empuja fotogramas completos al ring buffer.
- **Hilo de UI:** llama a `poll()` cada 100 ms. Nunca toca el audio directamente.

## Posición y cambios de pista

El decodificador anota en una lista (`boundaries`) en qué fotograma de salida empieza cada pista.
La UI compara con el contador de fotogramas que ha consumido el callback; así la posición y los
eventos `TrackStarted`/`Finished` corresponden a lo que se **oye**, no a lo que se ha decodificado.

Seek y cambio manual de pista: el decodificador pide un *flush* (`flush_req`), el callback vacía el
ring buffer y confirma (`flush_ack`). Cada carga lleva un `epoch` para descartar eventos de cargas anteriores.
El seek reabre el archivo en la posición pedida (simple y correcto aunque el decodificador ya haya
encadenado la pista siguiente).

## Gapless

Cuando el decodificador llega al final de un archivo, toma la pista de `next_slot` y sigue
escribiendo en el **mismo** ring buffer, sin silencio artificial. El controlador mantiene
`next_slot` actualizado con la siguiente pista según shuffle/repeat.

Symphonia se abre con `enable_gapless: true`, que recorta el retardo/relleno del codificador
cuando el formato lo informa.

| Formato | Comportamiento esperado |
|---|---|
| FLAC, WAV, AIFF | Sin relleno de codificador: continuidad exacta entre pistas del mismo formato. |
| OGG Vorbis | El recorte por granule position permite continuidad; a verificar con archivos reales. |
| MP3 | Gapless solo si el archivo trae cabecera LAME/Xing con retardo y relleno. Sin ella queda el relleno del codificador (hueco de decenas de ms). No se intenta ocultar. |

Límites de diseño conocidos:

- La pista siguiente se fija cuando la actual **empieza a sonar**; cambiar shuffle/repeat en la
  ventana de ~2 s en que el decodificador ya la tomó no la afecta.
- Si la pista encadenada falla al abrirse, el decodificador cierra el flujo (`End`) y el controlador
  salta a la siguiente; puede cortarse el final (≤ 1 s) de la pista anterior.

## Conversión

- **Frecuencia:** filtro sinc con ventana Kaiser (β = 9, ≈ 90 dB de rechazo): 80 coeficientes por
  muestra al subir de frecuencia (más al bajar), tabla polifásica de 512 fases con interpolación
  lineal entre fases y cada fase normalizada a ganancia 1. Medido con un tono de 10 kHz de
  44,1 → 48 kHz: error ≈ −15 dB con la interpolación lineal anterior (distorsión audible) frente a
  < −100 dB ahora. Al terminar cada pista `finish()` vacía la cola del filtro (~40 muestras) para
  que el empalme gapless no pierda el final.
- **Canales:** mono → N duplica; N → mono promedia; otros casos copian canales comunes y rellenan o descartan.
- **Formato de muestra de salida:** f32, i16 y u16.

## Volumen

La ganancia de normalización (ver más abajo) se aplica **antes** que este control.

El control es lineal (0–1) en la interfaz y se aplica como ganancia **cuadrática** (`v²`).

## Rampas anti-clic

El callback no aplica la ganancia de golpe: se acerca a la deseada con una rampa de ~12 ms por
muestra. Así **pausar, reanudar, mover el volumen y cambiar de pista o hacer seek** no producen
saltos de señal. En un cambio de pista se baja primero a cero y solo entonces se descarta el audio
viejo (`flush`). Tras cada carga el callback espera 60 ms de audio acumulado (*pre-roll*) antes de
sonar. Con el comportamiento anterior, estas acciones producían saltos de hasta media escala.

El callback además limita la salida a ±1,0 antes de convertir al formato del dispositivo.

## Prioridades de hilo

El hilo decodificador pide prioridad alta y el de escaneo de la biblioteca prioridad baja (si el
sistema no lo permite, se ignora). Para el diagnóstico, `underruns` cuenta solo los cortes reales.

## Portabilidad a Android

`auralis-audio` depende de Symphonia y CPAL, ambos sin APIs de Windows. CPAL admite Android
(requiere el backend correspondiente y comprobar el comportamiento del callback allí). `Engine`
no es `Send` porque el `Stream` de CPAL no lo es: debe crearse y usarse desde un único hilo.

## Varias carpetas y cola

El reproductor (`auralis-player`) tiene una sola cola: la de la carpeta que suena. Al tocar una canción
de otra carpeta la app llama a `set_tracks` con esa carpeta y empieza en la canción elegida; la cola
anterior (y su shuffle) se descarta. Es un cambio manual de pista, por lo que usa el mismo flush con
rampa anti-clic que cualquier cambio de pista. Los nombres editados solo existen en la interfaz: el
motor sigue usando la ruta real del archivo.

## Cambio de dispositivo de salida

El `Output` de CPAL queda atado al dispositivo con el que se abrió. Cada ~1 s `Player::tick` llama a
`Engine::needs_reopen()`, que es `true` si el callback de error del stream se disparó (`stream_lost`) o si
el nombre del dispositivo predeterminado del sistema difiere del que usamos. Entonces se crea un `Engine`
nuevo (nueva frecuencia/canales, nuevo ring buffer y decodificador), se reaplica el volumen, se recarga la
pista actual en la posición donde iba (`load(req, pos, autoplay)`) y se destruye el anterior. Si el motor
nuevo no se puede abrir, se conserva el viejo y se reintenta. Limitación: se compara el nombre del
dispositivo; dos dispositivos con el mismo nombre no se distinguen.

## Segundo plano / bandeja (Windows)

Con la ventana oculta o minimizada, Windows 10/11 puede tratar el proceso como «de segundo plano»
(EcoQoS): lo mueve a núcleos de bajo consumo, baja su frecuencia e ignora la resolución del temporizador.
El hilo decodificador (que además remuestrea con el filtro sinc) se retrasa, el ring buffer se vacía y el
callback rellena con silencio: se oye como chasquidos o cortes. Medidas (hipótesis sin medir en un equipo real):

- `power.rs` (app) llama a `SetProcessInformation(ProcessPowerThrottling)` pidiendo no ser limitado
  (velocidad de ejecución y resolución del temporizador).
- El hilo decodificador se registra en MMCSS («Pro Audio», `avrt.dll` cargada dinámicamente).
- El ring buffer pasa de 1 s a 2 s para tolerar más retraso. Efecto secundario: la ventana en que
  cambiar shuffle/repeat no afecta a la pista ya encadenada crece hasta ~2 s.

Para comprobarlo: pulsa la línea de estado (diagnóstico) y mira `underruns` antes y después de pasar un
rato en la bandeja; si sigue subiendo, el problema es otro.

## Normalización de volumen

`loudness.rs`: medidor BS.1770 (filtro K de dos etapas con coeficientes calculados para cualquier frecuencia,
bloques de 400 ms con paso de 100 ms, compuerta absoluta −70 LUFS y relativa −10 LU). Ganancia de pista =
`−14 − LUFS`, limitada a [−24, +12] dB y a que el pico de muestra no pase de −1 dBFS.

- La aplica el **hilo decodificador** sobre el audio ya convertido, antes del ring buffer, así que sigue
  correctamente alineada con los empalmes gapless y los crossfades; el callback no cambia. Al activar o
  desactivar se hace una rampa de un bloque (~24 ms).
- Si la pista no está en la caché, se mide al vuelo con los primeros 45 s (≈ decenas de ms) y se guarda como
  *parcial*; el hilo `auralis-loudness` (prioridad mínima) la sustituye por la medición completa. Solo las
  completas se guardan en disco (`loudness.txt`, con tamaño y fecha del archivo para invalidarlas si cambia).
- `Player::set_tracks` pide al hilo medir en segundo plano toda la lista; una petición nueva cancela la anterior.

## Crossfade

En el hilo decodificador. Cuando a la pista actual le quedan ≤ T ms (por su duración conocida) y hay pista
siguiente en `next_slot`, se abre la siguiente y se mezclan las dos con potencia constante
(`cos`/`sin` de t·π/2) en bloques de 2048 fotogramas, con limitador suave sobre 0,9. El inicio de la pista
nueva se anota en el fotograma donde empieza la mezcla (la interfaz cambia de título entonces). Si una pista
se queda sin audio durante la mezcla, la otra sigue con la curva intacta. `Load` (siguiente/anterior/seek)
cancela cualquier mezcla. Con T = 0 se usa el empalme gapless de siempre.

Límite: `next_slot` lo fija el controlador cuando la pista actual *suena*, ~2 s después de decodificarse
su inicio (tamaño del ring buffer); para canciones de duración < T + ~2 s la mezcla de su final puede
llegar tarde y reducirse o convertirse en un empalme sin mezcla.
