# hypr-dictate (Windows Edition)

Dictado por voz ligero adaptado para Windows: graba audio, envía la petición a un proxy OpenAI-compatible (como CLIProxyAPI) y escribe la transcripción directamente en la ventana enfocada simulando pulsaciones de teclado Unicode sin tocar el portapapeles.

## Novedades y Adaptación a Windows

- **Configuración de API Key desde la interfaz**: Ya no es obligatorio exportar variables de entorno. Puedes introducir, ver/ocultar y guardar tu clave API directamente en la pestaña **Servidor y API Key** de los Ajustes. Se guardará de forma persistente en `settings.json` y se recargará el catálogo de modelos automáticamente. (Sigue siendo compatible con `CLIPROXYAPI_API_KEY` como valor predeterminado si no se introduce manualmente).
- **Audio nativo en Windows (WASAPI via CPAL)**: Captura de micrófono nativa en Windows con re-muestreo dinámico a 16 kHz PCM mono 16-bit.
- **Inserción de texto nativa en Windows**: Simulación de teclado con `SendInput` (`KEYEVENTF_UNICODE`), respetando mayúsculas, tildes, caracteres internacionales, símbolos y emojis sin modificar el portapapeles.
- **Atajos globales de Windows**: Detección de atajos globales (F9, F10, CTRL + SHIFT + D, etc.) tanto en modo "Mantener para hablar" (Push-to-Talk) como en modo conmutador (Toggle).
- **Cancelación de audio en cualquier momento**:
  - Pulsa la tecla **Escape** mientras estás grabando o procesando para cancelar y descartar el audio inmediatamente.
  - O haz **clic derecho** sobre el dock mientras grabas.
  - O pulsa el botón rojo **[X]** que aparece a la derecha de la cápsula durante la grabación.
  - O ejecuta `./hypr-dictate.exe cancel` desde la terminal.
- **Dock 100% flotante (estilo widget/overlay)**:
  - Diseñado para flotar por encima de todas las ventanas (`HWND_TOPMOST` y `WS_EX_TOOLWINDOW`).
  - Compatible con gestores de ventanas en mosaico como **GlazeWM** (añadido a las reglas de exclusión para que nunca intente encajarlo en un mosaico).
- **Posicionamiento dinámico en pantalla**: Puedes anclar el dock en cualquier borde de tu monitor: **Abajo**, **Arriba**, **Izquierda** o **Derecha**, tanto desde los ajustes como arrastrándolo manualmente con el ratón.
- **Inicio automático con Windows**: Opción integrada para arrancar automáticamente al iniciar sesión en Windows (gestionada limpiamente en el registro `HKCU\...\Run`).
- **Interfaz gráfica moderna**: Dock flotante oscuro estilo cápsula con ondas de audio dinámicas en tiempo real animadas según la intensidad de la voz, y ventana de ajustes integrada (clic derecho o botón de engranaje).

## Compilar y Ejecutar

Requiere Rust en Windows. Para compilar:

```bash
cargo build --release
```

El ejecutable estará en `target/release/hypr-dictate.exe`.

Para ejecutarlo:

```bash
./target/release/hypr-dictate.exe
```

También soporta comandos por IPC desde la terminal mientras está en ejecución:

```bash
./target/release/hypr-dictate.exe start    # Inicia la grabación
./target/release/hypr-dictate.exe stop     # Detiene la grabación y transcribe
./target/release/hypr-dictate.exe cancel   # Cancela y descarta el audio actual
./target/release/hypr-dictate.exe toggle   # Alterna entre grabar / parar
./target/release/hypr-dictate.exe settings # Abre o cierra la ventana de ajustes
./target/release/hypr-dictate.exe reset    # Reinicia el estado del dock
```

## Configuración

Haz **clic derecho** sobre el dock o pulsa en el botón **⚙**:

1. **Pestaña Servidor y API Key**:
   - Introduce la URL base de tu servidor (por defecto `http://127.0.0.1:8317`).
   - Introduce tu **API Key** (con botón para mostrar/ocultar).
   - Pulsa **Guardar servidor y recargar catálogo**. Se conectará y listará todos los modelos disponibles.
2. **Pestaña General e IA**:
   - Selecciona la **Posición del Dock en la pantalla** (**Abajo**, **Arriba**, **Izquierda** o **Derecha**).
   - Selecciona el modelo que admita entrada de audio.
   - Elige el micrófono que desees usar (o deja el predeterminado de Windows).
   - Elige el atajo de teclado (por defecto `F9`) y si deseas usar "Mantener pulsado para hablar" (Push-to-Talk).
   - Activa o desactiva el **Inicio automático con Windows**.
   - Ajusta la temperatura y el prompt de transcripción.
   - Pulsa **Guardar cambios**.

Los ajustes se guardan en `%APPDATA%\hypr-dictate\settings.json`.
