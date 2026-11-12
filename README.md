# hypr-dictate

Dictado ligero para Hyprland: graba, envía una petición a CLIProxyAPI y escribe la transcripción completa en la ventana enfocada al comenzar. Rust y Wayland nativo, sin navegador ni historial; la inserción del dictado no usa el portapapeles.

## Compilar y ejecutar

Requiere Rust, `pkg-config` y las bibliotecas de desarrollo de GTK4 para compilar. En ejecución: GTK4, un tema de iconos simbólicos (por ejemplo, Adwaita), Hyprland con layer-shell y teclado virtual, PipeWire, `pw-record` y `hyprctl`.

```bash
cargo build --release
# La API key se puede introducir después desde los ajustes.
./target/release/hypr-dictate run
```

El caparazón oscuro aparece muy bajo y pegado al borde inferior. Al grabar se expande con una transición y muestra el nivel real de voz; al terminar de transcribir vuelve suavemente a su tamaño compacto. Clic izquierdo para grabar o detener; arrastre izquierdo para moverlo horizontalmente. Clic derecho abre o cierra una ventana de ajustes GTK4. Hyprland controla su posición y foco; el dock sigue siendo una superficie Wayland independiente. En reposo no captura audio ni mantiene animaciones activas.

Los ajustes usan controles GTK4 nativos: desplegables con búsqueda, campo de URL y editor de prompt con selección, portapapeles, deshacer y scroll. Tab permite recorrer los controles; Shift+Tab vuelve atrás. Los iconos simbólicos vienen del tema del sistema y acompañan etiquetas y ayudas, sin una biblioteca adicional de iconos.

En **General**, cada selector muestra las rutas completas del catálogo (por ejemplo, `cuenta/modelo`) y también modelos sin prefijo. No son credenciales verificadas; no todos los modelos aceptan audio. No se selecciona un modelo automáticamente. Pulsa **Guardar cambios** para aplicar modelo, atajo, micrófono, pantalla y parámetros de IA. La pantalla del dock cambia al guardar, no como vista previa. Cerrar por el botón o por el gestor de ventanas descarta los cambios sin guardar. Los ajustes, incluida la API key si decides guardarla, se guardan en `~/.config/hypr-dictate/settings.json` con permisos 0600 (solo tu usuario). La clave queda en texto plano, no en un almacén cifrado; no compartas ese archivo.

### Ajustes flotantes en Hyprland

La ventana de ajustes es una ventana GTK4 normal: Hyprland puede colocarla en mosaico. Para abrirla **flotante y centrada**, añade esta regla a tu configuración Lua de Hyprland (`~/.config/hypr/hyprland.lua` o un módulo cargado desde él):

```lua
hl.window_rule({
    name = "float-hypr-dictate-settings",
    match = { class = "^dev[.]j0k3r[.]HyprDictate$" },
    float = true,
    center = true,
})
```

Esta sintaxis se ha probado con **Hyprland 0.57 y configuración Lua**. Si usas otra versión o formato de configuración, adapta la regla a su sintaxis manteniendo la clase exacta `dev.j0k3r.HyprDictate`.

Ejecuta `hyprctl reload` y cierra y vuelve a abrir los ajustes para aplicar la regla. Conserva el tamaño predeterminado de la ventana y permite moverla o redimensionarla. Solo afecta a los ajustes, no al dock Wayland.

**La aplicación no instala esta regla automáticamente.** Es una preferencia local de Hyprland, independiente de `settings.json`.

### Servidor y modelo

La aplicación puede arrancar sin exportar ninguna clave. Configura servidor y autenticación desde la interfaz:

1. Abre los ajustes con clic derecho y entra en **Servidor y API key**.
2. Edita la URL base de CLIProxyAPI; por defecto es **`http://127.0.0.1:8317`**.
3. Desmarca **Usar CLIPROXYAPI_API_KEY** e introduce la clave. El campo la oculta y permite verla con el icono del ojo. Si el servidor no requiere autenticación, déjala vacía.
4. Pulsa **Guardar servidor y API key**. Se guardan en `settings.json` y se recarga el catálogo sin reiniciar, aunque el servidor anterior esté caído. No aplica otros cambios pendientes de la ventana. Cerrar sin guardar descarta la edición.

Si mantienes marcada **Usar CLIPROXYAPI_API_KEY**, se usa la variable heredada al arrancar (si existe) y no se copia su valor al archivo. Una clave manual, incluso vacía, tiene prioridad sobre la variable. La misma clave se usa para el catálogo y las transcripciones; guardar cambios generales no aplica una clave pendiente de guardar.

Se añaden `/v1/chat/completions` para transcribir y `/v1/models?client_version=1` para el catálogo. La base admite HTTP o HTTPS, una barra final opcional y un prefijo de ruta (por ejemplo, `https://proxy.example.com/cliproxy`); no incluyas `/v1`, credenciales, parámetros ni fragmentos. Una URL vacía o inválida no se guarda. No se usa una variable de entorno para la dirección.

Los ajustes anteriores sin `base_url` usan localhost. Una URL ya guardada se conserva. No se puede cambiar el servidor mientras se graba o procesa un dictado.

En **General** hay un único desplegable con búsqueda por función: **Modelo de dictado** y **Modelo de redacción profesional**. Cada uno muestra el catálogo completo con la ruta `cuenta/modelo`, sin selector de cuenta ni filtros por cuenta. Puedes elegir rutas distintas, siempre del mismo servidor y con la misma API key. Ambos modelos deben admitir entrada de audio.

Elige explícitamente un modelo del catálogo para cada modo que quieras usar y pulsa **Guardar cambios**. Puedes dejar uno sin configurar; ese modo pedirá elegir y guardar su modelo antes de grabar, sin utilizar el del otro. No se elige automáticamente una ruta ni una cuenta. El modelo guardado anteriormente se conserva como modelo de dictado; redacción queda sin modelo hasta que elijas uno. Cambiar el modelo de un modo no altera la selección del otro. Cerrar sin guardar descarta ambas selecciones pendientes.

## Dictado y redacción profesional

- **Dictado** mantiene el comportamiento y el prompt de transcripción actuales.
- **Redacción profesional** interpreta tu encargo hablado y escribe el texto que pides, en lugar de transcribir tus instrucciones. Por ejemplo, «haz un mensaje para Discord anunciando la prueba de hoy a las ocho, cercano y corto» debe producir el anuncio listo para pegar, sin incluir «haz un mensaje para Discord». Adapta canal, destinatario, tono y extensión; puede reorganizar y reformular las ideas, pero no inventar hechos. No publica ni envía nada, ni lee otros textos o el portapapeles: solo utiliza tu audio y el prompt de ese modo.

Los prompts ya guardados no se sustituyen automáticamente al actualizar. Para usar el nuevo comportamiento de redacción, pega el prompt nuevo en su editor y guarda; **Restaurar valores** restaura ambos prompts y también la temperatura. Comprueba con audio real el modelo elegido: las pruebas locales verifican el prompt enviado, no la calidad de la respuesta del modelo.

En **General**, activa **Redacción profesional al usar el dock** para usar ese modo con el botón del dock o `toggle`. También puedes configurar un **Atajo de redacción profesional** independiente del atajo de dictado; ambos empiezan desactivados en ajustes nuevos. Cada atajo mantiene su modo durante toda la grabación. En **Prompts e IA** puedes editar ambos prompts por separado; **Restaurar valores** los restaura en el borrador. Pulsa **Guardar cambios** para aplicar estas opciones.

La inserción de ambos modos sigue siendo texto plano en una sola línea: los saltos de línea se convierten en espacios por seguridad, nunca en pulsaciones de Enter.

## Atajos de mantener para hablar

Con el proceso principal ejecutándose:

```bash
./target/release/hypr-dictate start         # Dictado
./target/release/hypr-dictate start-formal  # Redacción profesional
./target/release/hypr-dictate stop
# Alternativa: toggle usa el modo elegido para el dock.
```

El panel permite elegir, para cada modo, entre combinaciones predefinidas (CTRL+SHIFT+D, CTRL+ALT+D, SUPER+SHIFT+D, SUPER+F9, SUPER+F10) o desactivar el atajo. Los dos modos deben tener atajos distintos. No es todavía un capturador de cualquier combinación de teclado.

Al guardar un cambio de atajo, comprueba conflictos en los bindings activos y modifica únicamente el bloque `-- BEGIN hypr-dictate` / `-- END hypr-dictate` de `~/.config/hypr/keybinds.lua`. Valida sintaxis con `luac -p`, recarga Hyprland y restaura el archivo si detecta errores de configuración. Requiere que ese archivo siga siendo el módulo cargado por tu configuración Lua. SUPER+D ya está ocupado en esta instalación y no se usa como valor predeterminado.

Suelta la tecla principal antes que los modificadores para que coincida el binding de liberación. No instala arranque automático; el proceso principal debe seguir activo y, si eliges autenticación por variable de entorno, heredar la clave. Una recarga externa puede afectar los bindings como cualquier otra configuración de Hyprland.

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
- El dock usa un `memfd` y un pool de dos buffers. Solo reutiliza un buffer cuando el compositor lo libera; si ambos están ocupados, espera sin sobrescribirlos. GTK4 gestiona por separado el dibujo y los eventos de la ventana de ajustes; no se redibuja manualmente con cada frame del dock.
- Los píxeles y los keymaps del teclado virtual usan `memfd`, sin archivos en `$TMPDIR`. Los buffers de píxeles no contienen audio, clave ni transcripciones; los keymaps sí contienen caracteres del texto, ordenados por primera aparición en cada bloque de hasta 10 caracteres distintos. La secuencia de bloques revela parte del orden de la transcripción. El sistema puede usar swap: no se garantiza borrado seguro de memoria.

## Validar

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

La prueba de widgets GTK está ignorada por defecto para no abrir ventanas durante `cargo test`; se ejecuta aparte con un display de pruebas aislado y el filtro `native_widgets_preserve -- --ignored --test-threads=1`. Se ha validado con Broadway, sin usar el compositor ni los ajustes reales.

Prueba manual pendiente en la sesión gráfica:

1. Abre un editor vacío, sitúa el cursor y activa el botón.
2. Dicta una frase en español con tildes y ñ; detén la grabación.
3. Cambia a otra ventana mientras procesa: debe volver al editor e insertar allí.
4. Comprueba el atajo de pulsar/soltar y que no roba el foco al empezar.
5. Cierra la ventana original durante otra petición: debe mostrar error y no escribir en la ventana alternativa.
6. En **Servidor y API key**, edita y guarda una URL y una clave válidas: debe recargar el catálogo sin reiniciar. Cierra y abre los ajustes para comprobar que se conservan. Edita otra URL o clave y cierra sin guardar: debe conservar las anteriores.
7. Con el servidor inaccesible, vuelve a **Servidor y API key** y corrige la URL o la clave. El guardado no debe depender del catálogo anterior; una URL inválida debe mostrar un error sin aplicarse.
8. Abre y cierra los ajustes varias veces mientras grabas o procesas: el dock debe seguir animándose, sin parpadeos ni bloqueos. En el prompt, prueba flechas, selección, Ctrl+A, copiar/pegar, deshacer, rueda y barra de scroll. Comprueba también la edición de la URL y el foco con Tab.
9. Cambia la pantalla del dock y cierra sin guardar: no debe moverse. Repite y guarda: debe moverse a la pantalla elegida. Comprueba que los iconos se muestran, que la ventana se puede redimensionar y que cerrar desde Hyprland descarta el borrador.

10. Configura modelos y atajos distintos para dictado y redacción y prueba ambos con el mismo mensaje. Comprueba que cada uno usa su modelo y prompt, que el modo del dock se aplica solo al guardar y que no se insertan saltos de línea como Enter. Elige una ruta `cuenta/modelo` distinta en un modo: no debe alterar el modelo del otro. Cierra sin guardar para comprobar que se descartan los cambios.

Las pruebas automáticas verifican estados, ajustes, prioridades de autenticación, cabeceras HTTP con servidor local, separación de prompts, generación de ambos bindings y widgets GTK; no sustituyen estas comprobaciones de audio, calidad del modelo y teclado virtual.
