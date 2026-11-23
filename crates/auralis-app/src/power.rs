//! Desactiva la limitación de energía de Windows 10/11 para este proceso.
//!
//! Con la ventana oculta o minimizada (bandeja), Windows puede marcar el proceso como
//! «de segundo plano»: lo pasa a núcleos de bajo consumo, baja su frecuencia y hace que
//! ignore la resolución del temporizador. Eso retrasa al hilo que decodifica y remuestrea
//! el audio y provoca cortes y chasquidos. Aquí se pide expresamente no ser limitado.
//! En otros sistemas no hace nada.

pub fn disable_power_throttling() {
    #[cfg(windows)]
    imp::disable();
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    #[repr(C)]
    struct ProcessPowerThrottlingState {
        version: u32,
        control_mask: u32,
        state_mask: u32,
    }

    const PROCESS_POWER_THROTTLING_CURRENT_VERSION: u32 = 1;
    const PROCESS_POWER_THROTTLING_EXECUTION_SPEED: u32 = 0x1;
    const PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION: u32 = 0x4;
    /// `PROCESS_INFORMATION_CLASS::ProcessPowerThrottling`
    const PROCESS_POWER_THROTTLING: i32 = 4;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn SetProcessInformation(
            process: *mut c_void,
            class: i32,
            info: *mut c_void,
            size: u32,
        ) -> i32;
    }

    pub fn disable() {
        // Máscara de control con el bit puesto y estado a 0 = «no limitar» esa característica.
        let mut state = ProcessPowerThrottlingState {
            version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            control_mask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED
                | PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
            state_mask: 0,
        };
        // SAFETY: `state` es válido y tiene el tamaño indicado; el resultado se ignora a
        // propósito (en Windows anteriores a 8 la función no existe en kernel32 y no se llega aquí
        // con un sistema soportado; si falla, la app funciona igual).
        unsafe {
            SetProcessInformation(
                GetCurrentProcess(),
                PROCESS_POWER_THROTTLING,
                &mut state as *mut _ as *mut c_void,
                std::mem::size_of::<ProcessPowerThrottlingState>() as u32,
            );
        }
    }
}
