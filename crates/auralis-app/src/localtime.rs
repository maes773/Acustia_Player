//! Fecha local (para agrupar las estadísticas por día).
//!
//! `std` no conoce la zona horaria, así que en Windows se pregunta al sistema
//! (`GetTimeZoneInformation`). En otros sistemas se usa UTC: solo afecta a quién
//! compile la app fuera de Windows.

use std::time::{SystemTime, UNIX_EPOCH};

/// Desfase local respecto a UTC, en segundos (positivo al este).
pub fn utc_offset_secs() -> i32 {
    imp::utc_offset_secs()
}

/// Número de día local (días desde 1970-01-01).
pub fn today() -> i32 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    auralis_core::stats::day_number(now, utc_offset_secs())
}

#[cfg(windows)]
mod imp {
    #[repr(C)]
    struct SystemTime {
        fields: [u16; 8],
    }

    #[repr(C)]
    struct TimeZoneInformation {
        bias: i32,
        standard_name: [u16; 32],
        standard_date: SystemTime,
        standard_bias: i32,
        daylight_name: [u16; 32],
        daylight_date: SystemTime,
        daylight_bias: i32,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetTimeZoneInformation(info: *mut TimeZoneInformation) -> u32;
    }

    pub fn utc_offset_secs() -> i32 {
        // SAFETY: estructura con el diseño de TIME_ZONE_INFORMATION, que el sistema rellena.
        let mut info: TimeZoneInformation = unsafe { std::mem::zeroed() };
        let kind = unsafe { GetTimeZoneInformation(&mut info) };
        // 0 = desconocido, 1 = hora estándar, 2 = horario de verano; 0xFFFFFFFF = error.
        let extra = match kind {
            1 => info.standard_bias,
            2 => info.daylight_bias,
            0 => 0,
            _ => return 0,
        };
        // Windows da «UTC = local + bias» en minutos, de ahí el signo.
        -(info.bias + extra) * 60
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn utc_offset_secs() -> i32 {
        0
    }
}
