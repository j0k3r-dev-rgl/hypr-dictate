# hypr-dictate

Dictado ligero para Hyprland: graba, envía una petición a CLIProxyAPI y escribe la transcripción completa en la ventana enfocada al comenzar. Rust y Wayland nativo, sin navegador, historial ni portapapeles.

## Compilar y ejecutar

Requiere Rust para compilar; en ejecución, Hyprland con layer-shell y teclado virtual, PipeWire, `pw-record` y `hyprctl`.

```bash
cargo build --release
# CLIPROXYAPI_API_KEY debe estar ya exportada en esta terminal.
./target/release/hypr-dictate run
```

El caparazón oscuro aparece muy bajo y pegado al borde inferior. Al grabar se expande con una transición y muestra el nivel real de voz; al terminar de transcribir vuelve suavemente a su tamaño compacto. Clic izquierdo para grabar o detener; arrastre izquierdo para moverlo horizontalmente. Clic derecho abre o cierra un modal centrado de configuración. En reposo no captura audio ni mantiene animaciones activas.

En el modal centrado, cada campo (cuenta/ruta, modelo y atajo) funciona como un selector desplegable: haz clic para abrir la lista y elegir una opción, o usa la rueda para recorrerla. «Automática» muestra modelos sin prefijo. Son rutas del catálogo, no credenciales verificadas; no todos los modelos aceptan audio. Pulsa **Guardar cambios** para aplicarlos. Las preferencias sin secretos se guardan en `~/.config/hypr-dictate/settings.json`.

### Servidor y modelo

La clave se lee de `CLIPROXYAPI_API_KEY`, nunca de un archivo de configuración. La URL se configura desde la interfaz:

1. Abre el modal con clic derecho y entra en **Servidor**.
2. Edita la URL base de CLIProxyAPI; por defecto es **`http://127.0.0.1:8317`**.
3. Pulsa **Guardar servidor**. Se guarda en `settings.json` y se recarga el catálogo sin reiniciar, aunque el servidor anterior esté caído. No aplica otros cambios pendientes del modal. Cerrar sin guardar descarta la edición.

Se añaden `/v1/chat/completions` para transcribir y `/v1/models?client_version=1` para el catálogo. La base admite HTTP o HTTPS, una barra final opcional y un prefijo de ruta (por ejemplo, `https://proxy.example.com/cliproxy`); no incluyas `/v1`, credenciales, parámetros ni fragmentos. Una URL vacía o inválida no se guarda. No se usa una variable de entorno para la dirección.

Los ajustes anteriores sin `base_url` usan localhost. Una URL ya guardada se conserva. No se puede cambiar el servidor mientras se graba o procesa un dictado.

Si no hay un modelo guardado, abre el modal, selecciona uno del catálogo que acepte audio y pulsa **Guardar cambios** antes de grabar. No se elige automáticamente una ruta ni una cuenta. Los modelos ya guardados se conservan.

## Atajo de mantener para hablar

Con el proceso principal ejecutándose:

```bash
./target/release/hypr-dictate start
./target/release/hypr-dictate stop
# Alternativa: toggle
```

El panel permite elegir entre combinaciones predefinidas (CTRL+SHIFT+D, CTRL+ALT+D, SUPER+SHIFT+D, SUPER+F9, SUPER+F10) o desactivar el atajo. No es todavía un capturador de cualquier combinación de teclado.

Al guardar un cambio de atajo, comprueba conflictos en los bindings activos y modifica únicamente el bloque `-- BEGIN hypr-dictate` / `-- END hypr-dictate` de `~/.config/hypr/keybinds.lua`. Valida sintaxis con `luac -p`, recarga Hyprland y restaura el archivo si detecta errores de configuración. Requiere que ese archivo siga siendo el módulo cargado por tu configuración Lua. SUPER+D ya está ocupado en esta instalación y no se usa como valor predeterminado.

Suelta la tecla principal antes que los modificadores para que coincida el binding de liberación. No instala arranque automático; el proceso principal debe seguir activo y heredar la clave. Una recarga externa puede afectar los bindings como cualquier otra configuración de Hyprland.

## Comportamiento y límites

- Captura PCM mono a 16 kHz en memoria, hasta cinco minutos (9,6 MB de PCM). Al alcanzar ese límite, la captura termina; debes detener el dictado para enviar lo capturado. WAV y base64 generan copias temporales adicionales en memoria.
- Al parar, envía una petición sin reintentos automáticos. El timeout total de transcripción es de 120 segundos e incluye subida, procesamiento del servidor y lectura de la respuesta; la conexión tiene un límite de 5 segundos. El catálogo mantiene un timeout total de 25 segundos y de conexión de 5 segundos. Si se agota el tiempo de transcripción, se informa como tal. La espera depende del servidor y la duración del audio; las pruebas previas de fragmentos cortos rondaron tres segundos, no tiempo real.
- Antes de insertar, comprueba que la ventana original sigue existiendo, la enfoca y verifica el foco. Si ya no existe, no escribe en otra ventana.
- Conserva la ventana, no la posición interna del cursor. No cambies de campo durante la petición. Las comprobaciones de foco reducen el riesgo, pero no son atómicas con la escritura: evita cambiar de ventana durante la inserción.
- **Primera prueba en un editor vacío, nunca en una terminal:** la inserción simula teclas y su interpretación depende de la aplicación. Los saltos de línea y tabulaciones se convierten en espacios y los demás caracteres de control se eliminan; no se envían como Enter ni Tab.
- Una sola grabación activa. Cancelar durante «Procesando» descarta el resultado y permite comenzar otra grabación, pero la petición HTTP anterior puede seguir en curso hasta terminar o agotar su timeout.
- La inserción usa bloques de hasta 10 caracteres Unicode distintos, sin un límite global de 240. La respuesta está limitada a 2 MB y la transcripción a 100 000 bytes UTF-8. Respuestas truncadas o inválidas no se insertan.
- No guarda audio ni texto. Un fallo de API o de inserción descarta el resultado; una interrupción durante la escritura puede dejar texto parcial, sin reintento para evitar duplicados.
- Con una URL base HTTP (incluido el valor predeterminado), clave y audio no están cifrados en tránsito: usa solo una red de confianza. Para cifrar el transporte, configura una URL HTTPS con un certificado válido; la validación TLS permanece activa y no se siguen redirecciones.
- Usa un socket local con permisos 0600 en `$XDG_RUNTIME_DIR/hypr-dictate.sock`. Si el proceso termina abruptamente puede quedar un socket residual. Elimínalo manualmente únicamente después de comprobar que no queda otra instancia.
- El dibujo usa un `memfd` y un pool de dos buffers por superficie. Solo reutiliza un buffer cuando el compositor lo libera; si ambos están ocupados, espera sin sobrescribirlos. El modal se repinta al cambiar su estado o el color del dictado, no por cada frame de animación del dock.
- Los píxeles y los keymaps del teclado virtual usan `memfd`, sin archivos en `$TMPDIR`. Los buffers de píxeles no contienen audio, clave ni transcripciones; los keymaps sí contienen caracteres del texto, ordenados por primera aparición en cada bloque de hasta 10 caracteres distintos. La secuencia de bloques revela parte del orden de la transcripción. El sistema puede usar swap: no se garantiza borrado seguro de memoria.

## Validar

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

Prueba manual pendiente en la sesión gráfica:

1. Abre un editor vacío, sitúa el cursor y activa el botón.
2. Dicta una frase en español con tildes y ñ; detén la grabación.
3. Cambia a otra ventana mientras procesa: debe volver al editor e insertar allí.
4. Comprueba el atajo de pulsar/soltar y que no roba el foco al empezar.
5. Cierra la ventana original durante otra petición: debe mostrar error y no escribir en la ventana alternativa.
6. En **Servidor**, edita y guarda una URL válida: debe recargar el catálogo sin reiniciar. Cierra y abre el modal para comprobar que se conserva. Edita otra URL y cierra sin guardar: debe conservar la anterior.
7. Con el servidor inaccesible, vuelve a **Servidor** y corrige la URL. El guardado no debe depender del catálogo anterior; una URL inválida debe mostrar un error sin aplicarse.
8. Abre y cierra el modal varias veces mientras grabas o procesas: el dock debe seguir animándose, sin parpadeos ni bloqueos. Edita un campo y comprueba que el modal se actualiza. Cambia la pantalla del dock y descarta la vista previa para comprobar la recreación de sus buffers.

Las pruebas automáticas verifican estados, validación del destino y cabecera WAV; no sustituyen estas comprobaciones de audio, API y teclado virtual.
