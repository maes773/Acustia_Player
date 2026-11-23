//! Ajustes específicos de Windows para el hilo decodificador. En otros sistemas no hacen nada.

/// Registra el hilo actual en MMCSS («Pro Audio»), el mecanismo de Windows para hilos de
/// audio: les da prioridad de planificación y los libera de la limitación de energía que
/// el sistema aplica a procesos en segundo plano (ventana oculta o minimizada).
/// `avrt.dll` se carga dinámicamente: si no existe, simplemente no se hace nada.
pub fn register_audio_thread() {
    #[cfg(windows)]
    imp::register();
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryW(name: *const u16) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    }

    type AvSetMmThreadCharacteristicsW =
        unsafe extern "system" fn(task: *const u16, index: *mut u32) -> *mut c_void;

    pub fn register() {
        let dll: Vec<u16> = "avrt.dll\0".encode_utf16().collect();
        let task: Vec<u16> = "Pro Audio\0".encode_utf16().collect();
        // SAFETY: llamadas a la API de Windows con cadenas terminadas en nulo que viven
        // durante toda la llamada; la firma de la función coincide con la documentada.
        unsafe {
            let module = LoadLibraryW(dll.as_ptr());
            if module.is_null() {
                return;
            }
            let f = GetProcAddress(module, b"AvSetMmThreadCharacteristicsW\0".as_ptr());
            if f.is_null() {
                return;
            }
            let f: AvSetMmThreadCharacteristicsW = std::mem::transmute(f);
            let mut index = 0u32;
            let _ = f(task.as_ptr(), &mut index);
        }
    }
}
