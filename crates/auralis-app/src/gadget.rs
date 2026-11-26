//! Comportamiento de «gadget de escritorio» en Windows.
//!
//! La ventana sin marco de Slint se convierte en un gadget de verdad con estilos nativos:
//! - `WS_EX_TOOLWINDOW`: no aparece en la barra de tareas ni en Alt+Tab.
//! - `WS_EX_NOACTIVATE`: hacer clic en sus botones no le quita el foco a la ventana en la
//!   que estés trabajando.
//! - Se mantiene al fondo del orden de ventanas (`HWND_BOTTOM`): queda sobre el escritorio
//!   y debajo de las demás ventanas (salvo que se active «siempre encima»).
//!
//! Se llama de forma periódica y es idempotente. Fuera de Windows no hace nada.
//! Limitación conocida: `Win+D` («mostrar escritorio») minimiza también el gadget.

/// Aplica (`enabled`) o quita el comportamiento de gadget. `on_top`: no enviarlo al fondo.
pub fn sync(enabled: bool, on_top: bool) {
    imp::sync(enabled, on_top);
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    type Hwnd = *mut c_void;

    const GWL_EXSTYLE: i32 = -20;
    const WS_EX_TOOLWINDOW: isize = 0x0000_0080;
    const WS_EX_NOACTIVATE: isize = 0x0800_0000;
    const SWP_NOSIZE: u32 = 0x0001;
    const SWP_NOMOVE: u32 = 0x0002;
    const SWP_NOZORDER: u32 = 0x0004;
    const SWP_NOACTIVATE: u32 = 0x0010;
    const SWP_FRAMECHANGED: u32 = 0x0020;
    const HWND_BOTTOM: Hwnd = 1 as Hwnd;
    const SW_HIDE: i32 = 0;
    const SW_SHOWNA: i32 = 8;
    const GW_OWNER: u32 = 4;

    #[link(name = "user32")]
    extern "system" {
        fn EnumWindows(cb: extern "system" fn(Hwnd, isize) -> i32, param: isize) -> i32;
        fn GetWindowThreadProcessId(hwnd: Hwnd, pid: *mut u32) -> u32;
        fn IsWindowVisible(hwnd: Hwnd) -> i32;
        fn GetWindow(hwnd: Hwnd, cmd: u32) -> Hwnd;
        fn ShowWindow(hwnd: Hwnd, cmd: i32) -> i32;
        fn SetWindowPos(
            hwnd: Hwnd,
            after: Hwnd,
            x: i32,
            y: i32,
            cx: i32,
            cy: i32,
            flags: u32,
        ) -> i32;
        #[cfg(target_pointer_width = "64")]
        fn GetWindowLongPtrW(hwnd: Hwnd, index: i32) -> isize;
        #[cfg(target_pointer_width = "64")]
        fn SetWindowLongPtrW(hwnd: Hwnd, index: i32, value: isize) -> isize;
        #[cfg(target_pointer_width = "32")]
        fn GetWindowLongW(hwnd: Hwnd, index: i32) -> i32;
        #[cfg(target_pointer_width = "32")]
        fn SetWindowLongW(hwnd: Hwnd, index: i32, value: i32) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcessId() -> u32;
    }

    #[cfg(target_pointer_width = "64")]
    unsafe fn get_ex(h: Hwnd) -> isize {
        GetWindowLongPtrW(h, GWL_EXSTYLE)
    }
    #[cfg(target_pointer_width = "64")]
    unsafe fn set_ex(h: Hwnd, v: isize) {
        SetWindowLongPtrW(h, GWL_EXSTYLE, v);
    }
    #[cfg(target_pointer_width = "32")]
    unsafe fn get_ex(h: Hwnd) -> isize {
        GetWindowLongW(h, GWL_EXSTYLE) as isize
    }
    #[cfg(target_pointer_width = "32")]
    unsafe fn set_ex(h: Hwnd, v: isize) {
        SetWindowLongW(h, GWL_EXSTYLE, v as i32);
    }

    struct Search {
        pid: u32,
        found: Hwnd,
    }

    extern "system" fn visit(hwnd: Hwnd, param: isize) -> i32 {
        // SAFETY: `param` es el puntero a `Search` que pasa `main_window` durante la llamada.
        let search = unsafe { &mut *(param as *mut Search) };
        let mut pid = 0u32;
        unsafe {
            GetWindowThreadProcessId(hwnd, &mut pid);
            // Ventana de nivel superior, visible y de este proceso (las ventanas ocultas de la
            // bandeja y de los atajos quedan fuera).
            if pid == search.pid
                && IsWindowVisible(hwnd) != 0
                && GetWindow(hwnd, GW_OWNER).is_null()
            {
                search.found = hwnd;
                return 0;
            }
        }
        1
    }

    fn main_window() -> Option<Hwnd> {
        let mut s = Search {
            pid: unsafe { GetCurrentProcessId() },
            found: std::ptr::null_mut(),
        };
        unsafe { EnumWindows(visit, &mut s as *mut Search as isize) };
        (!s.found.is_null()).then_some(s.found)
    }

    pub fn sync(enabled: bool, on_top: bool) {
        let Some(hwnd) = main_window() else { return };
        // SAFETY: `hwnd` es una ventana de este proceso recién encontrada.
        unsafe {
            let ex = get_ex(hwnd);
            let want = WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
            let has = ex & want == want;
            if enabled != has {
                let new = if enabled { ex | want } else { ex & !want };
                // Para que la barra de tareas se entere del cambio hay que ocultar y volver a
                // mostrar la ventana sin activarla.
                ShowWindow(hwnd, SW_HIDE);
                set_ex(hwnd, new);
                ShowWindow(hwnd, SW_SHOWNA);
                SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
                );
            }
            if enabled && !on_top {
                SetWindowPos(
                    hwnd,
                    HWND_BOTTOM,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn sync(_enabled: bool, _on_top: bool) {}
}
