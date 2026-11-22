# Motor de audio — notas técnicas (v0.0.1 Alpha)

## Flujo

```
UI ──Cmd──▶ hilo decodificador ──f32──▶ ring buffer (1 s) ──▶ callback CPAL
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
  ventana de ~1 s en que el decodificador ya la tomó no la afecta.
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
